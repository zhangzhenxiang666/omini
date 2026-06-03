use crate::api::LlmClient;
use crate::config::project::ProjectDir;
use crate::config::project::SessionDir;
use crate::engine::{QueryContext, QueryEngine, ToolPauseResolver};
use crate::mcp::McpManager;
use crate::permissions::PermissionEngine;
use crate::persistence::{RuntimePersistenceEvent, SessionRecord};
use crate::skills::SkillRegistry;
use crate::subagents::{AgentRegistry, RuntimeSubagentRunner};
use crate::tools::{ToolRegistry, ToolRuntimeContext};
use crate::types::config::Settings;
use crate::types::config::ThinkingEffort;
use crate::types::display::{DisplayMessage, DisplaySummary, HistoryItem, UserDraft};
use crate::types::events::{
    ActiveProfile, EngineToRuntimeEvent, LoadedSession, Notification, PlanApprovalAction,
    RuntimeToUiEvent, SessionUsageSnapshot, SubmittedPlan, ToolPauseKind, ToolPauseRequest,
    ToolPauseResponse, UiToRuntimeEvent,
};
use crate::types::message::Message;
use crate::types::usage::Usage;
use chrono::Utc;
use omini_domain::project::sanitize_project_path as sanitize;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use super::active_run;
use super::compact;
use super::history;
use super::plan;

#[derive(Debug)]
pub(super) enum RunStart {
    /// 启动前将最新 runtime 消息同时写入 LLM 历史和 UI 历史。
    UserMessage,
    /// 启动前将最新 runtime 消息写入 JSONL，将 UI-only display 消息写入 SQLite/UI 历史。
    SplitDisplayMessage { display_message: DisplayMessage },
    /// 基于现有历史继续运行，不新增用户消息。
    Continue,
}

#[derive(Debug)]
pub(super) struct CapabilityStore {
    subagents: RwLock<Arc<AgentRegistry>>,
    skills: RwLock<Arc<SkillRegistry>>,
}

impl CapabilityStore {
    fn load(settings: &Settings) -> Self {
        Self {
            subagents: RwLock::new(Arc::new(crate::subagents::load_agent_registry(
                &settings.cwd,
            ))),
            skills: RwLock::new(Arc::new(crate::skills::load_skill_registry(&settings.cwd))),
        }
    }

    pub(super) fn subagent_registry(&self) -> Arc<AgentRegistry> {
        self.subagents
            .read()
            .expect("subagent registry lock poisoned")
            .clone()
    }

    fn reload_subagents(&self, settings: &Settings) -> Arc<AgentRegistry> {
        let registry = Arc::new(crate::subagents::load_agent_registry(&settings.cwd));
        *self
            .subagents
            .write()
            .expect("subagent registry lock poisoned") = registry.clone();
        registry
    }

    pub(super) fn skill_registry(&self) -> Arc<SkillRegistry> {
        self.skills
            .read()
            .expect("skill registry lock poisoned")
            .clone()
    }
}

fn initial_display_message(start: &RunStart) -> Option<HistoryItem> {
    match start {
        RunStart::SplitDisplayMessage { display_message } => {
            Some(HistoryItem::Display(display_message.clone()))
        }
        RunStart::UserMessage | RunStart::Continue => None,
    }
}

/// Agent 运行时。
///
/// 维护自己的对话历史，通过 channel 与 UI 双向通信。
/// 一次 `UiToRuntimeEvent::SendMessage` 可能触发多轮 LLM 调用和工具执行，
/// 直到 LLM 自然结束或达到最大轮次。
pub struct AgentRuntime {
    /// 当前会话 ID，第一次提交时生成。
    pub(crate) session_id: Option<String>,
    /// 创建后缓存的会话目录句柄。
    pub(crate) session_dir: Option<SessionDir>,
    /// 向 UI 发送事件。
    event_tx: mpsc::Sender<RuntimeToUiEvent>,
    /// 向外部 server 转发 UI/SQLite 级持久化事件。
    persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
    /// 接收 UI 发来的请求。
    request_rx: mpsc::Receiver<UiToRuntimeEvent>,
    /// 运行时配置。
    pub(crate) settings: Settings,
    /// 当前项目目录。
    pub(crate) project: ProjectDir,
    /// 运行时自主维护的对话历史。
    pub(crate) messages: Vec<Message>,
    /// LLM 客户端。
    llm_client: LlmClient,
    /// 查询引擎。
    query_engine: QueryEngine,
    /// 工具注册表，持有所有注册的工具。
    tool_registry: Arc<ToolRegistry>,
    /// 从用户配置加载的 MCP 服务管理器。
    mcp_manager: Arc<McpManager>,
    /// runtime 是否已在 query 前等待过 MCP 启动。
    mcp_initialized: bool,
    /// runtime 侧的子代理生命周期服务。
    subagent_runner: Arc<RuntimeSubagentRunner>,
    /// runtime 管理的能力注册状态；每次 query 开始时生成只读快照。
    capabilities: CapabilityStore,
    /// 取消标志，用于 CancelRun。
    cancelled: Arc<AtomicBool>,
    /// 当前活跃 profile，供 runtime 主循环和运行中事件处理器共享读取。
    pub(crate) active_profile: Arc<RwLock<ActiveProfile>>,
    /// 当前 session 的 usage 快照；SQLite 落库由 server 处理。
    session_usage: Arc<Mutex<SessionUsageSnapshot>>,
}

impl AgentRuntime {
    pub fn new(
        event_tx: mpsc::Sender<RuntimeToUiEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        request_rx: mpsc::Receiver<UiToRuntimeEvent>,
        settings: Settings,
        project: ProjectDir,
    ) -> Self {
        Self::new_with_active_profile(
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project,
            ActiveProfile::Main,
        )
    }

    pub fn new_with_active_profile(
        event_tx: mpsc::Sender<RuntimeToUiEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        request_rx: mpsc::Receiver<UiToRuntimeEvent>,
        settings: Settings,
        project: ProjectDir,
        active_profile: ActiveProfile,
    ) -> Self {
        let mcp_manager = Arc::new(McpManager::from_settings(&settings));
        Self::with_mcp_manager(
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project,
            mcp_manager,
            active_profile,
        )
    }

    pub(crate) fn with_mcp_manager(
        event_tx: mpsc::Sender<RuntimeToUiEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        request_rx: mpsc::Receiver<UiToRuntimeEvent>,
        mut settings: Settings,
        project: ProjectDir,
        mcp_manager: Arc<McpManager>,
        active_profile: ActiveProfile,
    ) -> Self {
        let llm_client = LlmClient::new(
            settings.endpoint,
            settings.api_key.clone(),
            settings.base_url.clone(),
        );
        let tool_registry = Arc::new(crate::tools::create_main_registry());
        let subagent_runner = Arc::new(RuntimeSubagentRunner);
        let capabilities = CapabilityStore::load(&settings);
        let subagent_registry = capabilities.subagent_registry();
        let skill_registry = capabilities.skill_registry();
        settings.system_prompt = Some(crate::prompts::build_system_prompt_with_capabilities(
            &settings,
            &subagent_registry.summaries(),
            &skill_registry.injected_summaries(),
            active_profile,
        ));
        let permission_engine = Arc::new(PermissionEngine::load(
            settings.cwd.clone(),
            dirs::home_dir(),
            settings.permissions.clone(),
        ));

        // 向 UI 推送 runtime 侧能力快照，供自动补全使用。
        let _ = event_tx.try_send(RuntimeToUiEvent::AgentList(subagent_registry.summaries()));
        for diagnostic in &subagent_registry.diagnostics {
            let _ = event_tx.try_send(RuntimeToUiEvent::warning(format!(
                "Subagent: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in &skill_registry.diagnostics {
            let _ = event_tx.try_send(RuntimeToUiEvent::warning(format!(
                "Skill: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in permission_engine.diagnostics() {
            let _ = event_tx.try_send(RuntimeToUiEvent::warning(format!(
                "Permission: {diagnostic}"
            )));
        }

        Self {
            session_id: None,
            session_dir: None,
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project,
            messages: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            llm_client,
            tool_registry,
            mcp_manager,
            mcp_initialized: false,
            subagent_runner,
            capabilities,
            query_engine: QueryEngine::new(permission_engine),
            active_profile: Arc::new(RwLock::new(active_profile)),
            session_usage: Arc::new(Mutex::new(SessionUsageSnapshot::default())),
        }
    }

    /// 启动运行时，返回 JoinHandle。
    pub fn run(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.start_mcp_initialization();
            loop {
                tokio::select! {
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            UiToRuntimeEvent::SendMessage(draft) => {
                                self.submit_user_message(draft).await;
                            }
                            UiToRuntimeEvent::CompactContext { instructions } => {
                                self.compact_context(instructions.as_deref()).await;
                            }
                            UiToRuntimeEvent::SetThinkingEffort(effort) => {
                                active_run::apply_thinking_effort(
                                    &mut self.settings,
                                    &self.project,
                                    self.session_id.as_deref(),
                                    effort,
                                    &self.event_tx,
                                    &self.persistence_tx,
                                )
                                .await;
                            }
                            UiToRuntimeEvent::SetThinkingDisplay { show } => {
                                active_run::set_thinking_display(&self.project, show, &self.event_tx)
                                    .await;
                            }
                            UiToRuntimeEvent::ShutdownRequested => {
                                self.send_event(RuntimeToUiEvent::Shutdown).await;
                            }
                            UiToRuntimeEvent::ToggleActiveProfile => {
                                self.toggle_active_profile().await;
                            }
                            UiToRuntimeEvent::SetActiveProfile(profile) => {
                                self.set_active_profile(profile);
                                self.send_event(RuntimeToUiEvent::ActiveProfileChanged(
                                    self.active_profile(),
                                ))
                                .await;
                            }
                            UiToRuntimeEvent::InterveneMessage(draft) => {
                                let _ = draft;
                                self.send_event(RuntimeToUiEvent::error(
                                    "Cannot intervene because no run is active".to_string(),
                                ))
                                .await;
                            }
                            UiToRuntimeEvent::CancelRun => {
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.cancel_current_run();
                            }
                            UiToRuntimeEvent::ModelSelected { provider, model, thinking_effort } => {
                                self.switch_model(&provider, &model, thinking_effort).await;
                            }
                            UiToRuntimeEvent::SessionSelected { snapshot } => {
                                self.switch_session(snapshot).await;
                            }
                            UiToRuntimeEvent::AgentSaveRequested { source_kind, original_path, draft } => {
                                self.save_agent(source_kind, original_path.as_deref(), &draft).await;
                            }
                            UiToRuntimeEvent::AgentDeleteRequested { path } => {
                                self.delete_agent(&path).await;
                            }
                            UiToRuntimeEvent::AgentGenerateRequested { source_kind, description, tools, disallow_tools, model } => {
                                self.generate_agent(source_kind, &description, tools, disallow_tools, model).await;
                            }
                            UiToRuntimeEvent::ResolveToolPause { .. } => {
                                // 过期的权限响应可能在其他客户端已处理暂停、运行继续后抵达。
                            }
                            UiToRuntimeEvent::ResolvePlanApproval { plan_id, action } => {
                                self.resolve_plan_approval(&plan_id, action).await;
                            }
                        }
                    }
                    else => break,
                }
            }
        })
    }

    async fn compact_context(&mut self, custom_instructions: Option<&str>) {
        if let Err(error) = self
            .force_compact_current_session(custom_instructions)
            .await
        {
            self.send_event(RuntimeToUiEvent::Notification(Notification::warning(error)))
                .await;
        }
    }

    /// 切换模型 / 提供商，在 /model 交互完成后回调。
    async fn switch_model(
        &mut self,
        provider: &str,
        model: &str,
        thinking_effort: Option<ThinkingEffort>,
    ) {
        active_run::apply_model_selection(
            &mut self.settings,
            &mut self.llm_client,
            &self.project,
            self.session_id.as_deref(),
            active_run::ModelSelection {
                provider,
                model,
                thinking_effort,
            },
            active_run::RuntimeSinks {
                event_tx: &self.event_tx,
                persistence_tx: &self.persistence_tx,
                usage_state: &self.session_usage,
            },
        )
        .await;
    }

    /// 切换会话，在 /sessions 交互完成后回调。
    async fn switch_session(&mut self, snapshot: LoadedSession) {
        self.session_id = Some(snapshot.session_id.clone());

        let session_dir = self.project.session(&snapshot.session_id);
        self.session_dir = Some(session_dir.clone());

        // 同步会话的提供商 / 模型到运行时；若不同则切换。
        let provider_changed = snapshot.provider != self.settings.active_provider
            || snapshot.model != self.settings.model;

        if provider_changed && let Some(profile) = self.settings.providers.get(&snapshot.provider) {
            self.settings.active_provider = snapshot.provider.clone();
            self.settings.model = snapshot.model.clone();
            self.settings.api_key = profile.api_key.clone();
            self.settings.base_url = profile.base_url.clone();
            self.settings.endpoint = profile.endpoint;
            self.llm_client = LlmClient::new(
                profile.endpoint,
                profile.api_key.clone(),
                profile.base_url.clone(),
            );
        }

        // 同步思考强度。
        let thinking_effort = snapshot.thinking_effort;
        self.settings.thinking_effort = thinking_effort;
        self.set_active_profile(snapshot.active_profile);

        // UI 展示由 server 从 SQLite 提供；LLM 上下文使用 JSONL 历史。
        let runtime_messages = match session_dir.load_history() {
            Ok(messages) => messages,
            Err(e) => {
                self.send_event(RuntimeToUiEvent::warning(format!(
                    "加载 JSONL 历史失败，已降级使用 UI 消息快照: {e}"
                )))
                .await;
                snapshot
                    .messages
                    .iter()
                    .filter_map(|item| match item {
                        HistoryItem::Message(message) => Some(message.clone()),
                        HistoryItem::Display(_)
                        | HistoryItem::Plan(_)
                        | HistoryItem::Summary(_) => None,
                    })
                    .collect()
            }
        };

        self.messages = runtime_messages;
        let mut usage = snapshot.usage;
        usage.context_window = self.current_context_window();
        *self.session_usage.lock().await = usage;

        self.send_event(RuntimeToUiEvent::SessionTitleChanged {
            title: snapshot.title.clone(),
        })
        .await;

        self.send_event(RuntimeToUiEvent::ModelChanged {
            provider: snapshot.provider.clone(),
            model: snapshot.model.clone(),
            thinking_effort,
            context_window: self.current_context_window(),
        })
        .await;

        self.send_event(RuntimeToUiEvent::ActiveProfileChanged(
            self.active_profile(),
        ))
        .await;

        self.send_event(RuntimeToUiEvent::SessionChanged {
            session_id: Some(snapshot.session_id),
            messages: snapshot.messages,
            subagents: snapshot.subagents,
            usage,
        })
        .await;
    }

    async fn save_agent(
        &mut self,
        source_kind: crate::subagents::AgentSourceKind,
        original_path: Option<&std::path::Path>,
        draft: &crate::subagents::AgentDraft,
    ) {
        if crate::subagents::agent_name_exists(&self.settings.cwd, &draft.name, original_path) {
            self.send_event(RuntimeToUiEvent::error(format!(
                "agent '{}' 已存在",
                draft.name
            )))
            .await;
            return;
        }
        match crate::subagents::write_agent_file(&self.settings.cwd, source_kind, draft) {
            Ok(written_path) => {
                if let Some(path) = original_path
                    && path != written_path
                {
                    let _ = crate::subagents::delete_agent_file(path);
                }
                self.refresh_agents_after_change().await;
                self.send_event(RuntimeToUiEvent::notice(format!(
                    "agent '{}' 已保存",
                    draft.name
                )))
                .await;
            }
            Err(e) => self.send_event(RuntimeToUiEvent::error(e)).await,
        }
    }

    async fn delete_agent(&mut self, path: &std::path::Path) {
        match crate::subagents::delete_agent_file(path) {
            Ok(()) => {
                self.refresh_agents_after_change().await;
                self.send_event(RuntimeToUiEvent::notice("agent 已删除".to_string()))
                    .await;
            }
            Err(e) => self.send_event(RuntimeToUiEvent::error(e)).await,
        }
    }

    async fn refresh_agents_after_change(&mut self) {
        let registry = self.capabilities.reload_subagents(&self.settings);
        let skill_registry = self.capabilities.skill_registry();
        self.settings.system_prompt = Some(crate::prompts::build_system_prompt_with_capabilities(
            &self.settings,
            &registry.summaries(),
            &skill_registry.injected_summaries(),
            self.active_profile(),
        ));
        self.send_event(RuntimeToUiEvent::AgentList(registry.summaries()))
            .await;
        self.send_event(RuntimeToUiEvent::AgentManagementUpdated {
            records: crate::subagents::list_agent_records(&self.settings.cwd),
        })
        .await;
    }

    async fn generate_agent(
        &mut self,
        source_kind: crate::subagents::AgentSourceKind,
        description: &str,
        tools: Vec<String>,
        disallow_tools: Vec<String>,
        model: Option<String>,
    ) {
        match crate::subagents::generate_agent_draft(
            &self.llm_client,
            &self.settings,
            description,
            tools,
            disallow_tools,
            model,
        )
        .await
        {
            Ok(draft) => {
                self.send_event(RuntimeToUiEvent::AgentGenerated { source_kind, draft })
                    .await;
            }
            Err(e) => {
                self.send_event(RuntimeToUiEvent::AgentGenerateFailed { message: e })
                    .await;
            }
        }
    }

    pub(crate) async fn force_compact_current_session(
        &mut self,
        custom_instructions: Option<&str>,
    ) -> Result<(), String> {
        if self.messages.is_empty() {
            return Err("没有可压缩的会话历史".to_string());
        }
        if self.session_id.is_none() || self.session_dir.is_none() {
            return Err("当前没有已创建的会话，无法压缩历史".to_string());
        }

        let subagent_registry = self.capabilities.subagent_registry();
        let skill_registry = self.capabilities.skill_registry();
        let runtime_context = Arc::new(ToolRuntimeContext {
            session_id: self
                .session_id
                .clone()
                .expect("session id checked before compact"),
            session_type: "main".to_string(),
            agent_label: None,
            session_dir: self
                .session_dir
                .clone()
                .expect("session dir checked before compact"),
            subagent_registry,
            skill_registry,
            subagent_runner: Some(Arc::clone(&self.subagent_runner)),
            project: self.project.clone(),
        });
        let (compact_tx, mut compact_rx) = mpsc::channel(16);
        let event_tx = self.event_tx.clone();
        let persistence_tx = self.persistence_tx.clone();
        let usage_state = Arc::clone(&self.session_usage);
        let session_id = self
            .session_id
            .clone()
            .expect("session id checked before compact");
        let forwarder = tokio::spawn(async move {
            while let Some(event) = compact_rx.recv().await {
                match event {
                    EngineToRuntimeEvent::CompactShrinkStarted(_)
                    | EngineToRuntimeEvent::CompactShrinkFinished(_)
                    | EngineToRuntimeEvent::CompactShrinkFailed(_) => {
                        // TODO(compact): 收缩操作暂不通知 UI，后续再决定是否记录内部状态。
                    }
                    EngineToRuntimeEvent::CompactSummaryStarted(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryStarted(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryDelta(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryDelta(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFinished(event) => {
                        persist_compact_summary_event(&session_id, &event, &persistence_tx).await;
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryFinished(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryFailed(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage) => {
                        record_total_usage_and_notify(
                            &session_id,
                            usage,
                            &event_tx,
                            &persistence_tx,
                            &usage_state,
                        )
                        .await;
                    }
                    _ => {}
                }
            }
        });
        let tool_definitions = self.tool_registry_snapshot().definitions();
        let result = compact::force_compact(
            &mut self.messages,
            &self.settings,
            &self.llm_client,
            &tool_definitions,
            custom_instructions,
            Some(runtime_context),
            &compact_tx,
        )
        .await;
        drop(compact_tx);
        let _ = forwarder.await;

        match result {
            Ok(outcome) => {
                if compact_outcome_is_noop(&outcome) {
                    // 手动 compact 已经让 TUI 进入 working；无可压缩内容也要给一个终止事件。
                    self.send_event(RuntimeToUiEvent::warning(
                        "当前会话历史还不需要压缩".to_string(),
                    ))
                    .await;
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn set_active_profile(&mut self, profile: ActiveProfile) {
        *self
            .active_profile
            .write()
            .expect("active profile lock poisoned") = profile;
        self.rebuild_system_prompt();
    }

    pub(crate) fn active_profile(&self) -> ActiveProfile {
        *self
            .active_profile
            .read()
            .expect("active profile lock poisoned")
    }

    async fn toggle_active_profile(&mut self) {
        let next = match self.active_profile() {
            ActiveProfile::Main => ActiveProfile::Auto,
            ActiveProfile::Auto => ActiveProfile::Plan,
            ActiveProfile::Plan => ActiveProfile::Main,
        };
        self.set_active_profile(next);
        self.send_event(RuntimeToUiEvent::ActiveProfileChanged(
            self.active_profile(),
        ))
        .await;
    }

    fn rebuild_system_prompt(&mut self) {
        let active_profile = self.active_profile();
        active_run::rebuild_system_prompt(&mut self.settings, &self.capabilities, active_profile);
    }

    async fn resolve_plan_approval(&mut self, plan_id: &str, action: PlanApprovalAction) {
        match action {
            PlanApprovalAction::ContinueDiscussing => {
                self.set_active_profile(ActiveProfile::Plan);
                self.send_event(RuntimeToUiEvent::ActiveProfileChanged(
                    self.active_profile(),
                ))
                .await;
                self.send_plan_approval_resolved(plan_id, action).await;
            }
            PlanApprovalAction::Approve { profile } => {
                let plan_message = Message::from_user_text(plan::approval_message());
                self.send_plan_approval_resolved(plan_id, action).await;
                self.set_active_profile(profile.active_profile());
                self.send_event(RuntimeToUiEvent::ActiveProfileChanged(
                    self.active_profile(),
                ))
                .await;
                self.messages.push(plan_message.clone());
                self.send_event(RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(
                    plan_message,
                )))
                .await;
                self.process_run(RunStart::UserMessage).await;
            }
            PlanApprovalAction::ApproveAndCompact { profile } => {
                let path = self.plan_path(plan_id);
                let plan_content = match std::fs::read_to_string(&path) {
                    Ok(content) => content,
                    Err(e) => {
                        self.send_event(RuntimeToUiEvent::error(format!(
                            "无法压缩规划上下文，读取计划失败 {}: {e}",
                            path.display()
                        )))
                        .await;
                        return;
                    }
                };
                // 读到计划后才关闭所有客户端的审批抽屉，避免失败时丢掉可重试的 pending 计划。
                self.send_plan_approval_resolved(plan_id, action).await;
                let plan_message = Message::from_user_text(plan::compacted_context(&plan_content));
                self.session_id = None;
                self.session_dir = None;
                self.messages = vec![plan_message.clone()];
                self.set_active_profile(profile.active_profile());
                self.create_session(Some(HistoryItem::Message(plan_message.clone())))
                    .await;
                self.persist_compacted_plan_initial_message(plan_message)
                    .await;
                self.send_event(RuntimeToUiEvent::ActiveProfileChanged(
                    self.active_profile(),
                ))
                .await;
                self.process_run(RunStart::Continue).await;
            }
        }
    }

    async fn send_plan_approval_resolved(&self, plan_id: &str, action: PlanApprovalAction) {
        self.send_event(RuntimeToUiEvent::PlanApprovalResolved {
            plan_id: plan_id.to_string(),
            action,
        })
        .await;
    }

    fn plan_path(&self, plan_id: &str) -> std::path::PathBuf {
        plan::path(&self.project, plan_id)
    }

    async fn persist_compacted_plan_initial_message(&self, message: Message) {
        let Some(session_id) = self.session_id.as_deref() else {
            return;
        };
        let Some(session_dir) = self.session_dir.as_ref() else {
            return;
        };

        let blocks_dir = session_dir.path().join("blocks");
        history::persist_one(
            session_dir,
            session_id,
            &blocks_dir,
            message,
            &self.persistence_tx,
        )
        .await;
    }

    async fn persist_latest_proposed_plan(&self) -> Result<Option<SubmittedPlan>, String> {
        let submitted =
            plan::persist_latest(&self.project, self.active_profile(), &self.messages).await?;
        if let Some(plan) = submitted.as_ref()
            && let Some(session_id) = self.session_id.as_deref()
        {
            history::persist_plan_ui_message(session_id, plan, &self.persistence_tx).await;
        }
        Ok(submitted)
    }

    /// 接收一条用户消息，先回显给 UI，再启动运行。
    async fn submit_user_message(&mut self, draft: UserDraft) {
        let submission = match draft.into_submission() {
            Ok(submission) => submission,
            Err(error) => {
                self.send_event(RuntimeToUiEvent::error(error)).await;
                return;
            }
        };
        let history_item = submission.clone().history_item();
        self.messages.push(submission.llm_message);
        self.send_event(RuntimeToUiEvent::UserMessageInjected(history_item))
            .await;
        if let Some(display_message) = submission.display_message {
            self.process_run(RunStart::SplitDisplayMessage { display_message })
                .await;
        } else {
            self.process_run(RunStart::UserMessage).await;
        }
    }

    /// 处理一次完整的用户请求，可能包含多轮 LLM 调用。
    async fn process_run(&mut self, start: RunStart) {
        if self.session_id.is_none() {
            self.create_session(initial_display_message(&start)).await;
        } else {
            // 已有 session，更新 updated_at 时间戳。
            let id = self.session_id.as_ref().expect("session_id should exist");
            let _ = self
                .persistence_tx
                .send(RuntimePersistenceEvent::UpdateSessionUpdatedAt {
                    session_id: id.clone(),
                })
                .await;
        }

        history::persist_initial_user_message(
            self.session_id.as_deref(),
            self.session_dir.as_ref(),
            self.messages.last().cloned(),
            start,
            &self.persistence_tx,
        )
        .await;

        self.send_event(RuntimeToUiEvent::RunStarted).await;
        self.ensure_mcp_initialized().await;
        let tool_registry = self.tool_registry_snapshot();

        // 创建 engine -> runtime 的内部通信通道。
        let (engine_tx, engine_rx) = mpsc::channel::<EngineToRuntimeEvent>(256);
        let active_profile = self.active_profile();
        let active_profile_handle = Arc::clone(&self.active_profile);
        let tool_pause_resolver = self.query_engine.tool_pause_resolver();

        // 启动事件处理器独立 task，负责增量持久化和转发到 UI。
        let processor = self
            .spawn_event_processor(
                engine_rx,
                active_profile,
                Arc::clone(&active_profile_handle),
                tool_pause_resolver,
            )
            .await;

        {
            let subagent_registry = self.capabilities.subagent_registry();
            let skill_registry = self.capabilities.skill_registry();
            let run_settings = self.settings.clone();
            let run_settings = Arc::new(run_settings);
            // 引擎直接在当前 task 运行，让 &mut self.messages 保持零拷贝。
            let ctx = QueryContext {
                messages: &mut self.messages,
                settings: Arc::clone(&run_settings),
                llm_client: self.llm_client.clone(),
                tool_registry: Arc::clone(&tool_registry),
                active_profile,
                runtime_context: Some(Arc::new(ToolRuntimeContext {
                    session_id: self
                        .session_id
                        .clone()
                        .expect("session must exist before query"),
                    session_type: "main".to_string(),
                    agent_label: None,
                    session_dir: self
                        .session_dir
                        .clone()
                        .expect("session dir must exist before query"),
                    subagent_registry: Arc::clone(&subagent_registry),
                    skill_registry: Arc::clone(&skill_registry),
                    subagent_runner: Some(Arc::clone(&self.subagent_runner)),
                    project: self.project.clone(),
                })),
            };

            let event_tx = self.event_tx.clone();
            let query = self
                .query_engine
                .run_query(ctx, engine_tx, Arc::clone(&self.cancelled));
            tokio::pin!(query);

            loop {
                tokio::select! {
                    result = &mut query => {
                        let _result = result;
                        break;
                    }
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            UiToRuntimeEvent::CancelRun => {
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.cancel_current_run();
                            }
                            UiToRuntimeEvent::ResolveToolPause { tool_use_id, response } => {
                                if let Err(e) = self
                                    .query_engine
                                    .resolve_tool_pause(&tool_use_id, response)
                                {
                                    let _ = event_tx.send(RuntimeToUiEvent::error(e)).await;
                                }
                            }
                            UiToRuntimeEvent::InterveneMessage(draft) => {
                                let submission = match draft.into_submission() {
                                    Ok(submission) => submission,
                                    Err(error) => {
                                        let _ = event_tx.send(RuntimeToUiEvent::error(error)).await;
                                        continue;
                                    }
                                };
                                self.query_engine
                                    .enqueue_user_message(submission.llm_message);
                            }
                            UiToRuntimeEvent::ResolvePlanApproval { plan_id, action } => {
                                let _ = (plan_id, action);
                                let _ = event_tx
                                    .send(RuntimeToUiEvent::error(
                                        "Cannot resolve plan approval while a run is active".to_string(),
                                    ))
                                    .await;
                            }
                            UiToRuntimeEvent::SetThinkingEffort(effort) => {
                                active_run::apply_thinking_effort(
                                    &mut self.settings,
                                    &self.project,
                                    self.session_id.as_deref(),
                                    effort,
                                    &event_tx,
                                    &self.persistence_tx,
                                )
                                .await;
                            }
                            UiToRuntimeEvent::SetThinkingDisplay { show } => {
                                active_run::set_thinking_display(&self.project, show, &event_tx)
                                    .await;
                            }
                            UiToRuntimeEvent::ToggleActiveProfile => {
                                let mut active_profile = *active_profile_handle
                                    .read()
                                    .expect("active profile lock poisoned");
                                active_run::toggle_active_profile(
                                    &mut active_profile,
                                    &mut self.settings,
                                    &self.capabilities,
                                    &event_tx,
                                )
                                .await;
                                *active_profile_handle
                                    .write()
                                    .expect("active profile lock poisoned") = active_profile;
                            }
                            UiToRuntimeEvent::SetActiveProfile(profile) => {
                                if profile == ActiveProfile::Plan {
                                    active_run::reject_request(&event_tx).await;
                                } else {
                                    *active_profile_handle
                                        .write()
                                        .expect("active profile lock poisoned") = profile;
                                    active_run::rebuild_system_prompt(
                                        &mut self.settings,
                                        &self.capabilities,
                                        profile,
                                    );
                                    let _ = event_tx
                                        .send(RuntimeToUiEvent::ActiveProfileChanged(profile))
                                        .await;
                                }
                            }
                            UiToRuntimeEvent::ModelSelected { provider, model, thinking_effort } => {
                                active_run::apply_model_selection(
                                    &mut self.settings,
                                    &mut self.llm_client,
                                    &self.project,
                                    self.session_id.as_deref(),
                                    active_run::ModelSelection {
                                        provider: &provider,
                                        model: &model,
                                        thinking_effort,
                                    },
                                    active_run::RuntimeSinks {
                                        event_tx: &event_tx,
                                        persistence_tx: &self.persistence_tx,
                                        usage_state: &self.session_usage,
                                    },
                                )
                                .await;
                            }
                            UiToRuntimeEvent::SendMessage(_)
                            | UiToRuntimeEvent::CompactContext { .. }
                            | UiToRuntimeEvent::ShutdownRequested
                            | UiToRuntimeEvent::SessionSelected { .. }
                            | UiToRuntimeEvent::AgentSaveRequested { .. }
                            | UiToRuntimeEvent::AgentDeleteRequested { .. }
                            | UiToRuntimeEvent::AgentGenerateRequested { .. } => {
                                active_run::reject_request(&event_tx).await;
                            }
                        }
                    }
                    else => break,
                }
            }
        }

        // 等待事件处理器在 engine_tx drop 后自然退出。
        let _ = processor.await;

        self.cancelled.store(false, Ordering::Relaxed);
        self.send_event(RuntimeToUiEvent::RunFinished).await;

        match self.persist_latest_proposed_plan().await {
            Ok(Some(plan)) => {
                self.send_event(RuntimeToUiEvent::PlanSubmitted(plan)).await;
            }
            Ok(None) => {}
            Err(error) => {
                self.send_event(RuntimeToUiEvent::error(error)).await;
            }
        }
    }

    async fn ensure_mcp_initialized(&mut self) {
        if self.mcp_initialized {
            return;
        }
        self.mcp_initialized = true;

        if !self.mcp_manager.is_empty() {
            let _ = self.mcp_manager.initialize().await;
        }
    }

    fn tool_registry_snapshot(&self) -> Arc<ToolRegistry> {
        let mut registry = self.tool_registry.as_ref().clone();
        self.mcp_manager.register_available_tools(&mut registry);
        Arc::new(registry)
    }

    fn start_mcp_initialization(&self) {
        if self.mcp_manager.is_empty() {
            return;
        }

        let manager = Arc::clone(&self.mcp_manager);
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            for warning in manager.initialize().await {
                let _ = event_tx.send(RuntimeToUiEvent::warning(warning)).await;
            }
        });
    }

    /// 启动事件处理器。
    async fn spawn_event_processor(
        &self,
        mut engine_rx: mpsc::Receiver<EngineToRuntimeEvent>,
        active_profile: ActiveProfile,
        active_profile_handle: Arc<RwLock<ActiveProfile>>,
        tool_pause_resolver: ToolPauseResolver,
    ) -> tokio::task::JoinHandle<()> {
        let session_id = self
            .session_id
            .clone()
            .expect("session must exist before processing events");
        let session_dir = self
            .session_dir
            .clone()
            .expect("session dir must exist before processing events");
        let event_tx = self.event_tx.clone();
        let persistence_tx = self.persistence_tx.clone();
        let usage_state = Arc::clone(&self.session_usage);
        let project = self.project.clone();
        let blocks_dir = session_dir.path().join("blocks");
        let context_window = self.current_context_window();

        tokio::spawn(async move {
            let mut proposed_plan_forwarder = plan::ProposedPlanForwarder::new(active_profile);
            while let Some(event) = engine_rx.recv().await {
                match event {
                    // ===== 需要持久化的事件 =====
                    EngineToRuntimeEvent::UserMessageProduced(msg) => {
                        history::persist_one(
                            &session_dir,
                            &session_id,
                            &blocks_dir,
                            msg.clone(),
                            &persistence_tx,
                        )
                        .await;
                        let _ = event_tx
                            .send(RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(
                                msg,
                            )))
                            .await;
                    }
                    EngineToRuntimeEvent::LlmHistoryProduced(msg) => {
                        history::persist_llm_history_only(&session_dir, &msg);
                    }
                    EngineToRuntimeEvent::ToolResultsDisplayProduced(msg) => {
                        history::persist_ui_message(
                            &session_id,
                            &blocks_dir,
                            &msg,
                            &persistence_tx,
                        )
                        .await;
                    }
                    EngineToRuntimeEvent::MessageProduced(msg)
                    | EngineToRuntimeEvent::ToolResultsProduced(msg) => {
                        history::persist_one(
                            &session_dir,
                            &session_id,
                            &blocks_dir,
                            msg,
                            &persistence_tx,
                        )
                        .await;
                    }
                    // ===== 透传事件 =====
                    EngineToRuntimeEvent::TurnStarted => {
                        let _ = event_tx.send(RuntimeToUiEvent::TurnStarted).await;
                    }
                    EngineToRuntimeEvent::TurnEnded => {
                        proposed_plan_forwarder.flush(&event_tx).await;
                        let _ = event_tx.send(RuntimeToUiEvent::TurnEnded).await;
                    }
                    EngineToRuntimeEvent::ThinkingDelta(t) => {
                        let _ = event_tx.send(RuntimeToUiEvent::ThinkingDelta(t)).await;
                    }
                    EngineToRuntimeEvent::TextDelta(t) => {
                        proposed_plan_forwarder
                            .forward_text_delta(&event_tx, t)
                            .await;
                    }
                    EngineToRuntimeEvent::ToolUse(tu) => {
                        let _ = event_tx.send(RuntimeToUiEvent::ToolUse(tu)).await;
                    }
                    EngineToRuntimeEvent::ToolResult(tr) => {
                        let _ = event_tx.send(RuntimeToUiEvent::ToolResult(tr)).await;
                    }
                    EngineToRuntimeEvent::ToolPauseRequested(req) => {
                        if Self::should_auto_approve_permission_pause(&active_profile_handle, &req)
                        {
                            if let Err(e) = tool_pause_resolver.resolve_tool_pause(
                                &req.tool_use_id,
                                ToolPauseResponse::Permission {
                                    approved: true,
                                    note: None,
                                },
                            ) {
                                let _ = event_tx.send(RuntimeToUiEvent::error(e)).await;
                            }
                            continue;
                        }
                        let _ = event_tx
                            .send(RuntimeToUiEvent::ToolPauseRequested(req))
                            .await;
                    }
                    EngineToRuntimeEvent::PlanSubmitted(plan) => {
                        let _ = event_tx.send(RuntimeToUiEvent::PlanSubmitted(plan)).await;
                    }
                    EngineToRuntimeEvent::UsageRecorded(usage) => {
                        let _ = persistence_tx
                            .send(RuntimePersistenceEvent::RecordSessionUsage {
                                session_id: session_id.clone(),
                                usage,
                            })
                            .await;
                        let snapshot =
                            record_usage_snapshot(&usage_state, usage, context_window).await;
                        let _ = event_tx
                            .send(RuntimeToUiEvent::UsageChanged(snapshot))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactShrinkStarted(_)
                    | EngineToRuntimeEvent::CompactShrinkFinished(_)
                    | EngineToRuntimeEvent::CompactShrinkFailed(_) => {
                        // TODO(compact): 收缩操作暂不通知 UI，后续再决定是否记录内部状态。
                    }
                    EngineToRuntimeEvent::CompactSummaryStarted(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryStarted(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryDelta(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryDelta(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFinished(event) => {
                        persist_compact_summary_event(&session_id, &event, &persistence_tx).await;
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryFinished(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryFailed(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage) => {
                        record_total_usage_and_notify(
                            &session_id,
                            usage,
                            &event_tx,
                            &persistence_tx,
                            &usage_state,
                        )
                        .await;
                    }
                    EngineToRuntimeEvent::SubagentStarted(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::SubagentStarted(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentSessionCreated(session) => {
                        let _ = persistence_tx
                            .send(RuntimePersistenceEvent::CreateSession(session))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentUsageRecorded {
                        session_id: subagent_session_id,
                        usage,
                    } => {
                        let _ = persistence_tx
                            .send(RuntimePersistenceEvent::RecordSessionUsage {
                                session_id: subagent_session_id,
                                usage,
                            })
                            .await;
                        let _ = persistence_tx
                            .send(RuntimePersistenceEvent::RecordParentSubagentUsage {
                                session_id: session_id.clone(),
                                usage,
                            })
                            .await;
                        let snapshot =
                            record_total_usage_snapshot(&usage_state, usage, context_window).await;
                        let _ = event_tx
                            .send(RuntimeToUiEvent::UsageChanged(snapshot))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentMessageProduced(event) => {
                        let parent_dir = project.session(&session_id);
                        let subagent_dir = parent_dir.subagent(&event.session_id);
                        let subagent_blocks_dir = subagent_dir.path().join("blocks");
                        if event.persist_llm_history {
                            history::persist_one(
                                &subagent_dir,
                                &event.session_id,
                                &subagent_blocks_dir,
                                event.message.clone(),
                                &persistence_tx,
                            )
                            .await;
                        } else {
                            history::persist_ui_message(
                                &event.session_id,
                                &subagent_blocks_dir,
                                &event.message,
                                &persistence_tx,
                            )
                            .await;
                        }
                        let _ = event_tx
                            .send(RuntimeToUiEvent::SubagentMessageProduced(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentToolUse(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::SubagentToolUse(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentToolResult(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::SubagentToolResult(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentFinished(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::SubagentFinished(event))
                            .await;
                    }
                    EngineToRuntimeEvent::Error(e) => {
                        let _ = event_tx.send(RuntimeToUiEvent::error(e)).await;
                    }
                    EngineToRuntimeEvent::Warning(warning) => {
                        let _ = event_tx.send(RuntimeToUiEvent::warning(warning)).await;
                    }
                }
            }
        })
    }

    /// 发送事件到 UI，忽略 send 失败。
    pub(crate) async fn send_event(&self, event: RuntimeToUiEvent) {
        let _ = self.event_tx.send(event).await;
    }

    fn should_auto_approve_permission_pause(
        active_profile_handle: &RwLock<ActiveProfile>,
        req: &ToolPauseRequest,
    ) -> bool {
        let active_profile = *active_profile_handle
            .read()
            .expect("active profile lock poisoned");
        active_profile == ActiveProfile::Auto && matches!(req.kind, ToolPauseKind::Permission(_))
    }

    /// 首次提交时创建 session：生成 UUID、建目录，并把 UI 级索引事件转交给外部 server。
    async fn create_session(&mut self, initial_display_message: Option<HistoryItem>) {
        let id = Uuid::new_v4().to_string();
        self.session_id = Some(id.clone());

        let session_dir = self
            .project
            .create_session(&id)
            .expect("failed to create session directory");
        self.session_dir = Some(session_dir);

        let now = Utc::now();
        let project_path = sanitize(&self.settings.cwd);
        // 从第一条用户消息中提取标题。
        let title_text =
            history::title_text(initial_display_message.as_ref(), self.messages.last());
        let title = title_text.map(|text| text.chars().take(300).collect());
        let session = SessionRecord {
            id,
            project_path,
            parent_session_id: None,
            spawn_tool_use_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: self.settings.active_provider.clone(),
            model: self.settings.model.clone(),
            thinking_effort: self.settings.thinking_effort.map(|t| t.to_string()),
            title,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            created_at: now,
            updated_at: now,
        };
        if self
            .persistence_tx
            .send(RuntimePersistenceEvent::CreateSession(session.clone()))
            .await
            .is_err()
        {
            self.send_event(RuntimeToUiEvent::error(
                "Failed to forward session persistence event".to_string(),
            ))
            .await;
        }

        let title_out = session.title.clone();
        let session_id_out = session.id.clone();
        let usage = SessionUsageSnapshot {
            context_window: self.current_context_window(),
            ..SessionUsageSnapshot::default()
        };
        *self.session_usage.lock().await = usage;
        self.send_event(RuntimeToUiEvent::SessionTitleChanged { title: title_out })
            .await;
        self.send_event(RuntimeToUiEvent::SessionChanged {
            session_id: Some(session_id_out),
            messages: initial_display_message
                .map(|item| vec![item])
                .unwrap_or_else(|| {
                    self.messages
                        .clone()
                        .into_iter()
                        .map(HistoryItem::Message)
                        .collect()
                }),
            subagents: Vec::new(),
            usage,
        })
        .await;
    }

    fn current_context_window(&self) -> Option<u32> {
        active_run::current_context_window(&self.settings)
    }
}

async fn persist_compact_summary_event(
    session_id: &str,
    event: &crate::types::events::CompactSummaryFinishedEvent,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let summary = DisplaySummary {
        id: Uuid::new_v4().to_string(),
        title: "LLM Summary".to_string(),
        markdown: event.summary.clone(),
        created_at: Utc::now(),
    };
    history::persist_compact_summary_ui_message(session_id, &summary, persistence_tx).await;
}

fn compact_outcome_is_noop(outcome: &crate::runtime::compact::CompactOutcome) -> bool {
    outcome.before_tokens == outcome.after_tokens
        && outcome.before_messages == outcome.after_messages
}

async fn record_total_usage_and_notify(
    session_id: &str,
    usage: Usage,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::RecordSessionTotalUsage {
            session_id: session_id.to_string(),
            usage,
        })
        .await;
    let snapshot = record_total_usage_snapshot(usage_state, usage, None).await;
    let _ = event_tx
        .send(RuntimeToUiEvent::UsageTotalsChanged {
            total_tokens: snapshot.total_tokens,
            total_cached_tokens: snapshot.total_cached_tokens,
        })
        .await;
}

async fn record_usage_snapshot(
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
    usage: Usage,
    context_window: Option<u32>,
) -> SessionUsageSnapshot {
    let mut snapshot = usage_state.lock().await;
    let total_tokens = usage_tokens_i64(usage);
    let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
    snapshot.current_context_tokens = total_tokens;
    snapshot.total_tokens = snapshot.total_tokens.saturating_add(total_tokens);
    snapshot.total_cached_tokens = snapshot.total_cached_tokens.saturating_add(cached_tokens);
    snapshot.context_window = context_window;
    *snapshot
}

async fn record_total_usage_snapshot(
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
    usage: Usage,
    context_window: Option<u32>,
) -> SessionUsageSnapshot {
    let mut snapshot = usage_state.lock().await;
    let total_tokens = usage_tokens_i64(usage);
    let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
    snapshot.total_tokens = snapshot.total_tokens.saturating_add(total_tokens);
    snapshot.total_cached_tokens = snapshot.total_cached_tokens.saturating_add(cached_tokens);
    if context_window.is_some() {
        snapshot.context_window = context_window;
    }
    *snapshot
}

fn usage_tokens_i64(usage: Usage) -> i64 {
    usage_usize_to_i64(usage.total_tokens())
}

fn usage_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project::ProjectsDir;
    use crate::config::settings::{ModelEntry, ProviderConfig, UserConfig};
    use crate::types::config::{ProviderType, Settings};
    use crate::types::events::{NotificationKind, PlanExecutionProfile};
    use crate::types::message::{ContentBlock, Role};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    async fn ensure_test_persistence() {}

    fn test_persistence_tx() -> mpsc::Sender<RuntimePersistenceEvent> {
        let (tx, mut rx) = mpsc::channel(256);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        tx
    }

    fn test_persistence_channel() -> (
        mpsc::Sender<RuntimePersistenceEvent>,
        mpsc::Receiver<RuntimePersistenceEvent>,
    ) {
        mpsc::channel(256)
    }

    fn unique_temp_root(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("omini-{test_name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("failed to create temp test root");
        dir
    }

    fn test_user_config() -> UserConfig {
        let mut models = HashMap::new();
        models.insert(
            "gpt-test".to_string(),
            ModelEntry {
                name: None,
                limit: Some(256000),
                thinking: Some(true),
                input_modalities: None,
            },
        );

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                name: Some("OpenAI".to_string()),
                endpoint: ProviderType::OpenAI,
                base_url: "https://openai.example".to_string(),
                api_key: "test-key".to_string(),
                models: Some(models),
            },
        );

        UserConfig {
            providers,
            language: None,
            permissions: None,
            compact: None,
            mcp_servers: HashMap::new(),
        }
    }

    fn settings_for_cwd(config: &UserConfig, cwd: &Path) -> Settings {
        let mut settings = config
            .to_settings(None, None, None)
            .expect("failed to build settings");
        settings.cwd = cwd.to_path_buf();
        settings
    }

    fn text_content(message: &Message) -> &str {
        let Some(ContentBlock::Text(text)) = message.content.first() else {
            panic!("expected first content block to be text");
        };
        &text.text
    }

    fn drain_events(event_rx: &mut mpsc::Receiver<RuntimeToUiEvent>) -> Vec<RuntimeToUiEvent> {
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn permission_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(crate::types::events::PermissionPreview::Custom {
                tool_name: "bash".to_string(),
                payload: serde_json::Map::new(),
            }),
        }
    }

    fn empty_tool_pause_resolver() -> ToolPauseResolver {
        ToolPauseResolver::new(Arc::new(Mutex::new(HashMap::new())))
    }

    fn permission_tool_pause_resolver(
        tool_use_id: &str,
    ) -> (
        ToolPauseResolver,
        tokio::sync::oneshot::Receiver<ToolPauseResponse>,
    ) {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = tokio::sync::oneshot::channel();
        pending
            .lock()
            .expect("pending tool pause mutex poisoned")
            .insert(
                tool_use_id.to_string(),
                crate::tools::PendingToolPause::Permission(tx),
            );
        (ToolPauseResolver::new(pending), rx)
    }

    #[tokio::test]
    async fn submit_user_message_emits_ui_echo_before_run_starts() {
        ensure_test_persistence().await;

        let root = unique_temp_root("submit-user-message-echo");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, mut event_rx) = mpsc::channel(32);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        drain_events(&mut event_rx);

        runtime
            .submit_user_message(UserDraft::plain("hello".to_string()))
            .await;

        let events = drain_events(&mut event_rx);
        let injected = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(message))
                        if text_content(message) == "hello"
                )
            })
            .expect("user message should be echoed to UI");
        let started = events
            .iter()
            .position(|event| matches!(event, RuntimeToUiEvent::RunStarted))
            .expect("run should start");
        assert!(injected < started);
    }

    #[tokio::test]
    async fn toggle_active_profile_cycles_main_auto_plan() {
        let root = unique_temp_root("toggle-active-profile");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        assert_eq!(runtime.active_profile(), ActiveProfile::Main);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Auto);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Plan);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Main);

        let mut profiles = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::ActiveProfileChanged(profile) = event {
                profiles.push(profile);
            }
        }
        assert_eq!(
            profiles,
            vec![
                ActiveProfile::Auto,
                ActiveProfile::Plan,
                ActiveProfile::Main
            ]
        );
    }

    #[tokio::test]
    async fn active_run_profile_toggle_switches_main_and_auto_only() {
        let root = unique_temp_root("active-run-toggle-profile");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx.clone(),
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        drain_events(&mut event_rx);

        let mut active_profile = runtime.active_profile();
        active_run::toggle_active_profile(
            &mut active_profile,
            &mut runtime.settings,
            &runtime.capabilities,
            &event_tx,
        )
        .await;
        assert_eq!(active_profile, ActiveProfile::Auto);

        active_run::toggle_active_profile(
            &mut active_profile,
            &mut runtime.settings,
            &runtime.capabilities,
            &event_tx,
        )
        .await;
        assert_eq!(active_profile, ActiveProfile::Main);

        let profiles: Vec<_> = drain_events(&mut event_rx)
            .into_iter()
            .filter_map(|event| match event {
                RuntimeToUiEvent::ActiveProfileChanged(profile) => Some(profile),
                _ => None,
            })
            .collect();
        assert_eq!(profiles, vec![ActiveProfile::Auto, ActiveProfile::Main]);

        runtime.set_active_profile(ActiveProfile::Plan);
        let mut active_profile = runtime.active_profile();
        active_run::toggle_active_profile(
            &mut active_profile,
            &mut runtime.settings,
            &runtime.capabilities,
            &event_tx,
        )
        .await;
        assert_eq!(active_profile, ActiveProfile::Plan);

        let profiles: Vec<_> = drain_events(&mut event_rx)
            .into_iter()
            .filter_map(|event| match event {
                RuntimeToUiEvent::ActiveProfileChanged(profile) => Some(profile),
                _ => None,
            })
            .collect();
        assert!(profiles.is_empty());
    }

    #[tokio::test]
    async fn proposed_plan_block_is_persisted_as_submitted_plan() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "Intro\n<proposed_plan>\n# Durable Plan\n\n- Execute it.\n</proposed_plan>\nOutro"
                    .to_string(),
            )],
        )];

        let plan = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist proposed plan should succeed")
            .expect("plan should be extracted");

        assert_eq!(plan.title, "Durable Plan");
        assert_eq!(plan.id, "plan");
        assert_eq!(plan.markdown, "# Durable Plan\n\n- Execute it.");
        assert_eq!(plan.path, project.path().join("plans").join("plan.md"));
        assert_eq!(
            std::fs::read_to_string(&plan.path).expect("plan file should exist"),
            plan.markdown
        );
    }

    #[tokio::test]
    async fn proposed_plan_overwrites_current_plan_file() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-overwrite");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# First Plan\n\n- Earlier.\n</proposed_plan>".to_string(),
            )],
        )];
        let first = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist first proposed plan should succeed")
            .expect("first plan should be extracted");

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Second Plan\n\n- Later.\n</proposed_plan>".to_string(),
            )],
        )];
        let second = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist second proposed plan should succeed")
            .expect("second plan should be extracted");

        let expected_path = project.path().join("plans").join("plan.md");
        assert_eq!(first.id, "plan");
        assert_eq!(second.id, "plan");
        assert_eq!(first.path, expected_path);
        assert_eq!(second.path, expected_path);
        assert_eq!(
            std::fs::read_to_string(&second.path).expect("plan file should exist"),
            "# Second Plan\n\n- Later."
        );

        let entries = std::fs::read_dir(project.path().join("plans"))
            .expect("plans dir should exist")
            .collect::<Result<Vec<_>, _>>()
            .expect("plans dir should be readable");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path(), expected_path);
    }

    #[tokio::test]
    async fn proposed_plan_persistence_ignores_inline_tag_reference() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-inline-reference");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                concat!(
                    "好的，让我把完整计划整理成规范的 `<proposed_plan>` 块。\n\n",
                    "<proposed_plan>\n",
                    "# 添加 `/thinking` 命令\n\n",
                    "## 摘要\n\n",
                    "- 切换思考块展示。\n",
                    "</proposed_plan>",
                )
                .to_string(),
            )],
        )];

        let plan = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist proposed plan should succeed")
            .expect("plan should be extracted");

        assert_eq!(plan.title, "添加 `/thinking` 命令");
        assert!(plan.markdown.starts_with("# 添加 `/thinking` 命令"));
        assert!(!plan.markdown.starts_with("` 块。"));
        assert!(!plan.markdown.contains("<proposed_plan>"));
    }

    #[tokio::test]
    async fn proposed_plan_is_forwarded_as_plan_persistence_event() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-db");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# DB Plan\n\n- Keep style.\n</proposed_plan>".to_string(),
            )],
        )];

        let plan = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist proposed plan should succeed")
            .expect("plan should be extracted");

        let mut saw_plan_event = false;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertPlanMessage {
                session_id: event_session_id,
                plan: event_plan,
            } = event
            {
                assert_eq!(event_session_id, session_id);
                assert_eq!(event_plan.markdown, plan.markdown);
                saw_plan_event = true;
            }
        }
        assert!(saw_plan_event);
    }

    #[tokio::test]
    async fn compact_summary_is_forwarded_as_special_persistence_event() {
        ensure_test_persistence().await;

        let root = unique_temp_root("compact-summary-db");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            persistence_tx.clone(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        let event = crate::types::events::CompactSummaryFinishedEvent {
            trigger: crate::types::events::CompactTrigger::Manual,
            summary: "# Summary\n\n- Keep this.".to_string(),
            after_tokens: 42,
            session_id: Some(session_id.clone()),
            agent_label: None,
        };

        persist_compact_summary_event(&session_id, &event, &persistence_tx).await;

        let mut saw_summary_event = false;
        while let Ok(event_out) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertCompactSummaryMessage {
                session_id: event_session_id,
                summary,
            } = event_out
            {
                assert_eq!(event_session_id, session_id);
                assert_eq!(summary.markdown, event.summary);
                saw_summary_event = true;
            }
        }
        assert!(saw_summary_event);
    }

    #[tokio::test]
    async fn manual_compact_noop_emits_terminal_warning() {
        ensure_test_persistence().await;

        let root = unique_temp_root("compact-noop-warning");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let _ = drain_events(&mut event_rx);

        runtime
            .force_compact_current_session(None)
            .await
            .expect("noop compact should succeed");

        let events = drain_events(&mut event_rx);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                RuntimeToUiEvent::Notification(notification)
                    if notification.kind == NotificationKind::Warn
                        && notification.message.contains("还不需要压缩")
            )
        }));
    }

    #[tokio::test]
    async fn approve_plan_adds_short_user_confirmation_only() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-plan-short-message");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Approved plan\n\n- Execute it.\n</proposed_plan>".to_string(),
            )],
        )];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;

        runtime
            .resolve_plan_approval(
                "unused-plan-id",
                PlanApprovalAction::Approve {
                    profile: PlanExecutionProfile::Main,
                },
            )
            .await;

        let approval = runtime
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .expect("approval message should be added");
        assert_eq!(
            text_content(approval),
            "Approved. Implement the proposed plan now."
        );
        assert!(!text_content(approval).contains("# Approved plan"));

        let mut saw_short_approval_event = false;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(message)) = event
                && text_content(&message) == "Approved. Implement the proposed plan now."
            {
                saw_short_approval_event = true;
            }
        }
        assert!(saw_short_approval_event);
    }

    #[tokio::test]
    async fn approve_plan_can_start_in_auto_profile() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-plan-auto");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Approved plan\n\n- Execute it.\n</proposed_plan>".to_string(),
            )],
        )];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;

        runtime
            .resolve_plan_approval(
                "unused-plan-id",
                PlanApprovalAction::Approve {
                    profile: PlanExecutionProfile::Auto,
                },
            )
            .await;

        assert_eq!(runtime.active_profile(), ActiveProfile::Auto);
    }

    #[tokio::test]
    async fn resolve_plan_approval_broadcasts_resolved_event() {
        ensure_test_persistence().await;

        let root = unique_temp_root("resolve-plan-broadcast");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime
            .resolve_plan_approval("plan_1", PlanApprovalAction::ContinueDiscussing)
            .await;

        let mut saw_resolved = false;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::PlanApprovalResolved { plan_id, action } = event {
                assert_eq!(plan_id, "plan_1");
                assert_eq!(action, PlanApprovalAction::ContinueDiscussing);
                saw_resolved = true;
            }
        }
        assert!(saw_resolved);
    }

    #[tokio::test]
    async fn approve_and_compact_creates_new_session_with_plan_as_initial_user_message() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-compact");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::from_user_text("old conversation".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let old_session_id = runtime.session_id.clone();

        let plan_id = "plan";
        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("failed to create plans dir");
        std::fs::write(
            plans_dir.join("plan.md"),
            "# Approved plan\n\n1. Execute it.",
        )
        .expect("failed to write plan");

        runtime
            .resolve_plan_approval(
                plan_id,
                PlanApprovalAction::ApproveAndCompact {
                    profile: PlanExecutionProfile::Main,
                },
            )
            .await;

        let new_session_id = runtime.session_id.clone();
        assert_ne!(new_session_id, old_session_id);
        assert_eq!(runtime.active_profile(), ActiveProfile::Main);
        assert_eq!(runtime.messages.len(), 1);
        assert_eq!(runtime.messages[0].role, Role::User);
        assert!(
            text_content(&runtime.messages[0]).contains("Implement the plan in a fresh context")
        );
        assert!(text_content(&runtime.messages[0]).contains("re-read files as needed"));
        assert!(text_content(&runtime.messages[0]).contains("Approved plan:"));
        assert!(text_content(&runtime.messages[0]).contains("# Approved plan"));

        let session_dir = runtime
            .session_dir
            .as_ref()
            .expect("new session dir should exist");
        assert_eq!(
            session_dir
                .load_history()
                .expect("failed to load persisted history"),
            runtime.messages
        );

        let mut saw_new_session_event = false;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::SessionChanged {
                session_id: Some(session_id),
                messages,
                ..
            } = event
            {
                if Some(session_id) == new_session_id {
                    assert_eq!(messages.len(), 1);
                    saw_new_session_event = true;
                }
            }
        }
        assert!(saw_new_session_event);
    }

    #[tokio::test]
    async fn approve_and_compact_can_start_new_session_in_auto_profile() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-compact-auto");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::from_user_text("old conversation".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let old_session_id = runtime.session_id.clone();

        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("failed to create plans dir");
        std::fs::write(
            plans_dir.join("plan.md"),
            "# Approved plan\n\n1. Execute it.",
        )
        .expect("failed to write plan");

        runtime
            .resolve_plan_approval(
                "plan",
                PlanApprovalAction::ApproveAndCompact {
                    profile: PlanExecutionProfile::Auto,
                },
            )
            .await;

        assert_ne!(runtime.session_id, old_session_id);
        assert_eq!(runtime.active_profile(), ActiveProfile::Auto);
        assert_eq!(runtime.messages.len(), 1);
        assert!(text_content(&runtime.messages[0]).contains("Approved plan:"));
    }

    #[tokio::test]
    async fn switch_session_resets_active_profile_to_main_and_notifies_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("switch-session-mode");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::from_user_text("session body".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        while event_rx.try_recv().is_ok() {}

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime
            .switch_session(LoadedSession {
                session_id: session_id.clone(),
                provider: runtime.settings.active_provider.clone(),
                model: runtime.settings.model.clone(),
                thinking_effort: runtime.settings.thinking_effort,
                active_profile: ActiveProfile::Main,
                title: Some("session body".to_string()),
                messages: vec![HistoryItem::Message(runtime.messages[0].clone())],
                subagents: Vec::new(),
                usage: SessionUsageSnapshot::default(),
            })
            .await;

        assert_eq!(runtime.active_profile(), ActiveProfile::Main);
        assert!(
            runtime
                .settings
                .system_prompt
                .as_deref()
                .expect("system prompt should be rebuilt")
                .contains("<core_behavior>")
        );
        assert!(
            !runtime
                .settings
                .system_prompt
                .as_deref()
                .expect("system prompt should be rebuilt")
                .contains("<plan_mode_instructions>")
        );

        let mut saw_main_mode_event = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(
                event,
                RuntimeToUiEvent::ActiveProfileChanged(ActiveProfile::Main)
            ) {
                saw_main_mode_event = true;
            }
        }
        assert!(saw_main_mode_event);
    }

    #[tokio::test]
    async fn event_processor_auto_profile_resolves_permission_pause_without_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("auto-profile-pause-runtime");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        runtime.create_session(None).await;
        runtime.set_active_profile(ActiveProfile::Auto);
        drain_events(&mut event_rx);

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let (tool_pause_resolver, permission_rx) = permission_tool_pause_resolver("tool_1");
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                tool_pause_resolver,
            )
            .await;

        engine_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(permission_pause(
                "tool_1",
            )))
            .await
            .expect("pause event should send");

        let response = tokio::time::timeout(Duration::from_secs(1), permission_rx)
            .await
            .expect("auto permission response should arrive")
            .expect("auto permission waiter should stay open");
        assert_eq!(
            response,
            ToolPauseResponse::Permission {
                approved: true,
                note: None,
            }
        );
        assert!(
            !drain_events(&mut event_rx)
                .into_iter()
                .any(|event| matches!(event, RuntimeToUiEvent::ToolPauseRequested(_)))
        );

        drop(engine_tx);
        processor.await.expect("processor should finish");
    }

    #[tokio::test]
    async fn event_processor_main_profile_forwards_permission_pause_to_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("main-profile-pause-ui");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        runtime.create_session(None).await;
        drain_events(&mut event_rx);

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;

        engine_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(permission_pause(
                "tool_1",
            )))
            .await
            .expect("pause event should send");

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("ui pause event should arrive")
            .expect("ui event channel should stay open");
        let RuntimeToUiEvent::ToolPauseRequested(req) = event else {
            panic!("expected tool pause event");
        };
        assert_eq!(req.tool_use_id, "tool_1");

        drop(engine_tx);
        processor.await.expect("processor should finish");
    }

    #[tokio::test]
    async fn event_processor_forwards_produced_user_message_to_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("produced-user-message-ui");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime =
            AgentRuntime::new(event_tx, persistence_tx, request_rx, settings, project);
        runtime.create_session(None).await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        drain_events(&mut event_rx);

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;
        let message = Message::from_user_text("intervention".to_string());

        engine_tx
            .send(EngineToRuntimeEvent::UserMessageProduced(message.clone()))
            .await
            .expect("user message event should send");

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("ui user message event should arrive")
            .expect("ui event channel should stay open");
        let RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(event_message)) = event
        else {
            panic!("expected user message injection");
        };
        assert_eq!(event_message, message);

        let mut saw_persistence_event = false;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertMessage {
                session_id: event_session_id,
                role,
                blocks,
                ..
            } = event
                && event_session_id == session_id
                && role == "user"
                && blocks == message.content
            {
                saw_persistence_event = true;
            }
        }
        assert!(saw_persistence_event);

        drop(engine_tx);
        processor.await.expect("processor should finish");
    }

    #[tokio::test]
    async fn split_tool_result_history_writes_image_only_to_jsonl() {
        ensure_test_persistence().await;

        let root = unique_temp_root("split-tool-result-history");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime =
            AgentRuntime::new(event_tx, persistence_tx, request_rx, settings, project);
        runtime.create_session(None).await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        let session_dir = runtime
            .session_dir
            .clone()
            .expect("session dir should exist");

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;

        let display_msg = Message::new(
            Role::User,
            vec![ContentBlock::from_tool_result(
                "toolu_image".to_string(),
                false,
                "Loaded image".to_string(),
            )],
        );
        let llm_msg = Message::new(
            Role::User,
            vec![
                ContentBlock::from_tool_result(
                    "toolu_image".to_string(),
                    false,
                    "Loaded image".to_string(),
                ),
                ContentBlock::from_base64_image("image/png".to_string(), "abc123".to_string()),
            ],
        );

        engine_tx
            .send(EngineToRuntimeEvent::LlmHistoryProduced(llm_msg.clone()))
            .await
            .expect("llm history event should send");
        engine_tx
            .send(EngineToRuntimeEvent::ToolResultsDisplayProduced(
                display_msg.clone(),
            ))
            .await
            .expect("display history event should send");
        drop(engine_tx);
        processor.await.expect("processor should finish");

        assert_eq!(session_dir.load_history().unwrap(), vec![llm_msg]);

        let mut saw_display_message = false;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertMessage {
                session_id: event_session_id,
                blocks,
                ..
            } = event
                && event_session_id == session_id
                && blocks == display_msg.content
            {
                assert!(
                    !blocks
                        .iter()
                        .any(|block| matches!(block, ContentBlock::Image(_)))
                );
                saw_display_message = true;
            }
        }
        assert!(saw_display_message);
    }

    #[tokio::test]
    async fn usage_events_update_main_and_subagent_session_totals() {
        ensure_test_persistence().await;

        let root = unique_temp_root("usage-events");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime =
            AgentRuntime::new(event_tx, persistence_tx, request_rx, settings, project);

        runtime.messages = vec![Message::from_user_text("session body".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let parent_session_id = runtime.session_id.clone().expect("session id should exist");
        drain_events(&mut event_rx);
        while persistence_rx.try_recv().is_ok() {}

        let subagent_session_id = Uuid::new_v4().to_string();

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;

        engine_tx
            .send(EngineToRuntimeEvent::UsageRecorded(
                crate::types::usage::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cached_tokens: 3,
                },
            ))
            .await
            .expect("usage event should send");
        engine_tx
            .send(EngineToRuntimeEvent::SubagentUsageRecorded {
                session_id: subagent_session_id.clone(),
                usage: crate::types::usage::Usage {
                    prompt_tokens: 7,
                    completion_tokens: 8,
                    cached_tokens: 4,
                },
            })
            .await
            .expect("subagent usage event should send");
        drop(engine_tx);
        processor.await.expect("processor should finish");

        let mut saw_parent_usage = false;
        let mut saw_subagent_usage = false;
        let mut saw_parent_subagent_usage = false;
        while let Ok(event) = persistence_rx.try_recv() {
            match event {
                RuntimePersistenceEvent::RecordSessionUsage { session_id, usage }
                    if session_id == parent_session_id =>
                {
                    assert_eq!(usage.total_tokens(), 15);
                    assert_eq!(usage.cached_tokens, 3);
                    saw_parent_usage = true;
                }
                RuntimePersistenceEvent::RecordSessionUsage { session_id, usage }
                    if session_id == subagent_session_id =>
                {
                    assert_eq!(usage.total_tokens(), 15);
                    assert_eq!(usage.cached_tokens, 4);
                    saw_subagent_usage = true;
                }
                RuntimePersistenceEvent::RecordParentSubagentUsage { session_id, usage }
                    if session_id == parent_session_id =>
                {
                    assert_eq!(usage.total_tokens(), 15);
                    assert_eq!(usage.cached_tokens, 4);
                    saw_parent_subagent_usage = true;
                }
                _ => {}
            }
        }
        assert!(saw_parent_usage);
        assert!(saw_subagent_usage);
        assert!(saw_parent_subagent_usage);

        let mut last_usage = None;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::UsageChanged(snapshot) = event {
                last_usage = Some(snapshot);
            }
        }
        let snapshot = last_usage.expect("usage snapshot should be emitted");
        assert_eq!(snapshot.current_context_tokens, 15);
        assert_eq!(snapshot.total_tokens, 30);
        assert_eq!(snapshot.total_cached_tokens, 7);
    }
}
