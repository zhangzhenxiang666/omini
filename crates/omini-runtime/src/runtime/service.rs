use crate::api::LlmClient;
use crate::command::{self, CommandRegistry};
use crate::config::project::ProjectDir;
use crate::config::project::SessionDir;
use crate::config::project::sanitize;
use crate::db;
use crate::engine::{QueryContext, QueryEngine};
use crate::permissions::PermissionEngine;
use crate::subagents::{AgentRegistry, RuntimeSubagentRunner};
use crate::tools::{ToolRegistry, ToolRuntimeContext};
use crate::types::config::Settings;
use crate::types::config::ThinkingEffort;
use crate::types::display::{DisplayMessage, HistoryItem};
use crate::types::events::{
    CommandEffect, CommandResult, EngineToRuntimeEvent, InteractionRequest, RuntimeToUiEvent,
    UiToRuntimeEvent,
};
use crate::types::message::Message;
use chrono::Utc;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::history;

/// 待处理的交互类型（等待 UI 回传选择结果）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PendingInteraction {
    ModelSelect,
    SessionSelect,
    AgentManage,
}

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
struct CapabilityStore {
    subagents: RwLock<Arc<AgentRegistry>>,
}

impl CapabilityStore {
    fn load(settings: &Settings) -> Self {
        Self {
            subagents: RwLock::new(Arc::new(crate::subagents::load_agent_registry(
                &settings.cwd,
            ))),
        }
    }

    fn subagent_registry(&self) -> Arc<AgentRegistry> {
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
/// 一次 `UiToRuntimeEvent::SendMessage` 可能触发多轮 LLM 调用 + 工具执行，
/// 直到 LLM 自然结束或达到最大轮次。
pub struct AgentRuntime {
    /// 当前会话 ID（第一次提交时生成）
    pub(crate) session_id: Option<String>,
    /// 创建后缓存会话目录句柄
    pub(crate) session_dir: Option<SessionDir>,
    /// 向 UI 发送事件
    event_tx: mpsc::Sender<RuntimeToUiEvent>,
    /// 接收 UI 发来的请求
    request_rx: mpsc::Receiver<UiToRuntimeEvent>,
    /// 配置
    pub(crate) settings: Settings,
    /// 当前项目目录
    pub(crate) project: ProjectDir,
    /// 运行时自主维护的对话历史
    pub(crate) messages: Vec<Message>,
    /// LLM 客户端
    llm_client: LlmClient,
    /// 查询引擎
    query_engine: QueryEngine,
    /// 工具注册表（持有所有注册的工具）
    tool_registry: Arc<ToolRegistry>,
    /// Runtime-side subagent lifecycle service.
    subagent_runner: Arc<RuntimeSubagentRunner>,
    /// Runtime 管理的能力注册状态；每次 query 开始时生成只读快照。
    capabilities: CapabilityStore,
    /// 取消标志（用于 CancelRun）
    cancelled: Arc<AtomicBool>,
    /// 命令注册表
    pub(crate) command_registry: CommandRegistry,
    /// 当前等待 UI 回传的交互类型
    pending_interaction: Option<PendingInteraction>,
}

impl AgentRuntime {
    pub fn new(
        event_tx: mpsc::Sender<RuntimeToUiEvent>,
        request_rx: mpsc::Receiver<UiToRuntimeEvent>,
        mut settings: Settings,
        project: ProjectDir,
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
        settings.system_prompt = Some(crate::prompts::build_system_prompt_with_subagents(
            &settings,
            &subagent_registry.summaries(),
        ));
        let permission_engine = Arc::new(PermissionEngine::load(
            settings.cwd.clone(),
            dirs::home_dir(),
            settings.permissions.clone(),
        ));

        // 初始化命令注册表并注册内置命令
        let mut command_registry = CommandRegistry::new();
        command::register_default_commands(&mut command_registry);

        // 向 UI 推送 runtime 侧能力快照（供自动补全使用）
        let _ = event_tx.try_send(RuntimeToUiEvent::CommandList(command_registry.summaries()));
        let _ = event_tx.try_send(RuntimeToUiEvent::AgentList(subagent_registry.summaries()));
        for diagnostic in &subagent_registry.diagnostics {
            let _ = event_tx.try_send(RuntimeToUiEvent::Warning(format!(
                "Subagent: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in permission_engine.diagnostics() {
            let _ = event_tx.try_send(RuntimeToUiEvent::Warning(format!(
                "Permission: {diagnostic}"
            )));
        }

        Self {
            session_id: None,
            session_dir: None,
            event_tx,
            request_rx,
            settings,
            project,
            messages: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            llm_client,
            tool_registry,
            subagent_runner,
            capabilities,
            query_engine: QueryEngine::new(permission_engine),
            command_registry,
            pending_interaction: None,
        }
    }

    /// 启动运行时，返回 JoinHandle。
    pub fn run(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            UiToRuntimeEvent::SendMessage(draft) => {
                                let submission = match draft.into_submission() {
                                    Ok(submission) => submission,
                                    Err(error) => {
                                        self.send_event(RuntimeToUiEvent::Error(error)).await;
                                        continue;
                                    }
                                };
                                self.messages.push(submission.llm_message);
                                if let Some(display_message) = submission.display_message {
                                    self.process_run(RunStart::SplitDisplayMessage { display_message }).await;
                                } else {
                                    self.process_run(RunStart::UserMessage).await;
                                }
                            }
                            UiToRuntimeEvent::SendCommand(text) => {
                                if let Some(parsed) = command::parse(&text) {
                                    self.handle_command(&parsed).await;
                                }
                            }
                            UiToRuntimeEvent::InterveneMessage(draft) => {
                                let _ = draft;
                                self.send_event(RuntimeToUiEvent::Error(
                                    "Cannot intervene because no run is active".to_string(),
                                ))
                                .await;
                            }
                            UiToRuntimeEvent::CancelRun => {
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.cancel_current_run();
                            }
                            UiToRuntimeEvent::ModelSelected { provider, model, thinking_effort } => {
                                if self.pending_interaction
                                    == Some(PendingInteraction::ModelSelect)
                                {
                                    self.switch_model(&provider, &model, thinking_effort).await;
                                    self.pending_interaction = None;
                                }
                            }
                            UiToRuntimeEvent::SessionSelected { session_id } => {
                                if self.pending_interaction
                                    == Some(PendingInteraction::SessionSelect)
                                {
                                    self.switch_session(&session_id).await;
                                    self.pending_interaction = None;
                                }
                            }
                            UiToRuntimeEvent::AgentSaveRequested { source_kind, original_path, draft } => {
                                if self.pending_interaction == Some(PendingInteraction::AgentManage) {
                                    self.save_agent(source_kind, original_path.as_deref(), &draft).await;
                                }
                            }
                            UiToRuntimeEvent::AgentDeleteRequested { path } => {
                                if self.pending_interaction == Some(PendingInteraction::AgentManage) {
                                    self.delete_agent(&path).await;
                                }
                            }
                            UiToRuntimeEvent::AgentGenerateRequested { source_kind, description, tools, disallow_tools, model } => {
                                if self.pending_interaction == Some(PendingInteraction::AgentManage) {
                                    self.generate_agent(source_kind, &description, tools, disallow_tools, model).await;
                                }
                            }
                            UiToRuntimeEvent::ResolveToolPause { .. } => {
                                self.send_event(RuntimeToUiEvent::Error(
                                    "Cannot resolve tool pause because no run is active".to_string(),
                                ))
                                .await;
                            }
                        }
                    }
                    else => break,
                }
            }
        })
    }

    /// 处理命令分发。
    async fn handle_command(&mut self, parsed: &command::ParsedCommand<'_>) {
        if let Some(cmd) = self.command_registry.get(parsed.name) {
            let cmd = Arc::clone(cmd);
            let result = cmd.execute(self, parsed.args).await;
            match result {
                CommandResult::Ok(effects) => {
                    for effect in effects {
                        self.apply_command_effect(effect).await;
                    }
                }
                CommandResult::Error(e) => {
                    self.send_event(RuntimeToUiEvent::Error(e)).await;
                }
            }
        } else {
            self.send_event(RuntimeToUiEvent::CommandNotice(format!(
                "未知命令: /{}. 输入 /help 查看可用命令。",
                parsed.name
            )))
            .await;
        }
    }

    async fn apply_command_effect(&mut self, effect: CommandEffect) {
        match effect {
            CommandEffect::Notice(text) => {
                self.send_event(RuntimeToUiEvent::CommandNotice(text)).await;
            }
            CommandEffect::ShowInteraction(req) => {
                self.pending_interaction = match &req {
                    InteractionRequest::ModelSelection { .. } => {
                        Some(PendingInteraction::ModelSelect)
                    }
                    InteractionRequest::SessionSelection { .. } => {
                        Some(PendingInteraction::SessionSelect)
                    }
                    InteractionRequest::AgentManagement { .. } => {
                        Some(PendingInteraction::AgentManage)
                    }
                };
                self.send_event(RuntimeToUiEvent::InteractionRequest(req))
                    .await;
            }
            CommandEffect::InjectUserMessage(msg) => {
                self.messages.push(msg.clone());
                self.send_event(RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(
                    msg,
                )))
                .await;
                self.process_run(RunStart::UserMessage).await;
            }
            CommandEffect::InjectUserQuery {
                llm_message,
                display_message,
            } => {
                self.messages.push(llm_message);
                self.send_event(RuntimeToUiEvent::UserMessageInjected(HistoryItem::Display(
                    display_message.clone(),
                )))
                .await;
                self.process_run(RunStart::SplitDisplayMessage { display_message })
                    .await;
            }
            CommandEffect::ContinueQuery => {
                self.process_run(RunStart::Continue).await;
            }
            CommandEffect::Emit(event) => {
                self.send_event(*event).await;
            }
        }
    }

    /// 切换模型 / 提供商（/model 交互完成后回调）。
    async fn switch_model(
        &mut self,
        provider: &str,
        model: &str,
        thinking_effort: Option<ThinkingEffort>,
    ) {
        if let Some(profile) = self.settings.providers.get(provider) {
            self.settings.active_provider = provider.to_string();
            self.settings.model = model.to_string();
            self.settings.thinking_effort = thinking_effort;
            self.settings.api_key = profile.api_key.clone();
            self.settings.base_url = profile.base_url.clone();
            self.settings.endpoint = profile.endpoint;

            // 重建 LLM 客户端
            self.llm_client = LlmClient::new(
                profile.endpoint,
                profile.api_key.clone(),
                profile.base_url.clone(),
            );

            // 持久化：新会话 → 项目状态；已有会话 → 数据库会话记录
            if let Some(sid) = &self.session_id {
                let te = thinking_effort.map(|t| t.to_string());
                let _ = db::global_db()
                    .update_session_config(sid, provider, model, te.as_deref())
                    .await;
            } else if let Ok(mut state) = self.project.load_state() {
                state.default_provider = Some(provider.to_string());
                state.default_model = Some(model.to_string());
                state.thinking_effort = thinking_effort;
                let _ = self.project.save_state(&state);
            }

            // 通知 UI
            self.send_event(RuntimeToUiEvent::ModelChanged {
                provider: provider.to_string(),
                model: model.to_string(),
                thinking_effort: self.settings.thinking_effort,
            })
            .await;
        } else {
            self.send_event(RuntimeToUiEvent::Error(format!(
                "提供商 '{provider}' 不存在"
            )))
            .await;
        }
    }

    /// 切换会话（/sessions 交互完成后回调）。
    async fn switch_session(&mut self, session_id: &str) {
        self.session_id = Some(session_id.to_string());

        let db_session = match db::global_db().get_session(session_id).await {
            Ok(Some(s)) => s,
            _ => {
                self.send_event(RuntimeToUiEvent::Error("会话不存在".to_string()))
                    .await;
                return;
            }
        };

        let session_dir = self.project.session(session_id);
        self.session_dir = Some(session_dir.clone());

        // 同步会话的提供商 / 模型到运行时（若不同则切换）
        let provider_changed = db_session.provider != self.settings.active_provider
            || db_session.model != self.settings.model;

        if provider_changed && let Some(profile) = self.settings.providers.get(&db_session.provider)
        {
            self.settings.active_provider = db_session.provider.clone();
            self.settings.model = db_session.model.clone();
            self.settings.api_key = profile.api_key.clone();
            self.settings.base_url = profile.base_url.clone();
            self.settings.endpoint = profile.endpoint;
            self.llm_client = LlmClient::new(
                profile.endpoint,
                profile.api_key.clone(),
                profile.base_url.clone(),
            );
        }

        // 同步思考程度
        let thinking_effort = db_session
            .thinking_effort
            .as_deref()
            .and_then(|s| s.parse::<ThinkingEffort>().ok());
        self.settings.thinking_effort = thinking_effort;

        // UI 展示使用数据库消息；LLM 上下文使用 JSONL 历史。
        let blocks_dir = session_dir.path().join("blocks");
        let ui_messages = history::load_messages_from_db(session_id, &blocks_dir).await;
        let runtime_messages = match session_dir.load_history() {
            Ok(messages) => messages,
            Err(e) => {
                self.send_event(RuntimeToUiEvent::Warning(format!(
                    "加载 JSONL 历史失败，已降级使用数据库消息: {e}"
                )))
                .await;
                ui_messages
                    .iter()
                    .filter_map(|item| match item {
                        HistoryItem::Message(message) => Some(message.clone()),
                        HistoryItem::Display(_) => None,
                    })
                    .collect()
            }
        };
        let subagents = history::load_subagents_for_session(session_id, &self.project).await;

        let count = ui_messages.len() as i64;
        self.messages = runtime_messages;

        let _ = db::global_db()
            .update_session_msg_count(session_id, count)
            .await;

        self.send_event(RuntimeToUiEvent::SessionTitleChanged {
            title: db_session.title,
        })
        .await;

        self.send_event(RuntimeToUiEvent::ModelChanged {
            provider: db_session.provider.clone(),
            model: db_session.model.clone(),
            thinking_effort,
        })
        .await;

        self.send_event(RuntimeToUiEvent::SessionChanged {
            session_id: Some(session_id.to_string()),
            messages: ui_messages,
            subagents,
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
            self.send_event(RuntimeToUiEvent::Error(format!(
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
                self.send_event(RuntimeToUiEvent::CommandNotice(format!(
                    "agent '{}' 已保存",
                    draft.name
                )))
                .await;
            }
            Err(e) => self.send_event(RuntimeToUiEvent::Error(e)).await,
        }
    }

    async fn delete_agent(&mut self, path: &std::path::Path) {
        match crate::subagents::delete_agent_file(path) {
            Ok(()) => {
                self.refresh_agents_after_change().await;
                self.send_event(RuntimeToUiEvent::CommandNotice("agent 已删除".to_string()))
                    .await;
            }
            Err(e) => self.send_event(RuntimeToUiEvent::Error(e)).await,
        }
    }

    async fn refresh_agents_after_change(&mut self) {
        let registry = self.capabilities.reload_subagents(&self.settings);
        self.settings.system_prompt = Some(crate::prompts::build_system_prompt_with_subagents(
            &self.settings,
            &registry.summaries(),
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

    /// 处理一次完整的用户请求（可能含多轮 LLM 调用）。
    async fn process_run(&mut self, start: RunStart) {
        if self.session_id.is_none() {
            self.create_session(initial_display_message(&start)).await;
        } else {
            // 已有 session，更新 updated_at 时间戳
            let id = self.session_id.as_ref().expect("session_id should exist");
            let _ = db::global_db().update_session_updated_at(id).await;
        }

        history::persist_initial_user_message(
            self.session_id.as_deref(),
            self.session_dir.as_ref(),
            self.messages.last().cloned(),
            start,
        )
        .await;

        self.send_event(RuntimeToUiEvent::RunStarted).await;

        // 创建 engine → runtime 的内部通信通道
        let (engine_tx, engine_rx) = mpsc::channel::<EngineToRuntimeEvent>(256);

        // 启动事件处理器（独立 task），负责增量持久化 + 转发到 UI
        let processor = self.spawn_event_processor(engine_rx).await;

        {
            let subagent_registry = self.capabilities.subagent_registry();
            let run_settings = self.settings.clone();
            let run_settings = Arc::new(run_settings);
            // 引擎直接在当前 task 运行，&mut self.messages 零拷贝
            let ctx = QueryContext {
                messages: &mut self.messages,
                settings: Arc::clone(&run_settings),
                llm_client: self.llm_client.clone(),
                tool_registry: Arc::clone(&self.tool_registry),
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
                                    let _ = event_tx.send(RuntimeToUiEvent::Error(e)).await;
                                }
                            }
                            UiToRuntimeEvent::InterveneMessage(draft) => {
                                let submission = match draft.into_submission() {
                                    Ok(submission) => submission,
                                    Err(error) => {
                                        let _ = event_tx.send(RuntimeToUiEvent::Error(error)).await;
                                        continue;
                                    }
                                };
                                self.query_engine
                                    .enqueue_user_message(submission.llm_message);
                            }
                            UiToRuntimeEvent::SendMessage(_)
                            | UiToRuntimeEvent::SendCommand(_)
                            | UiToRuntimeEvent::ModelSelected { .. }
                            | UiToRuntimeEvent::SessionSelected { .. }
                            | UiToRuntimeEvent::AgentSaveRequested { .. }
                            | UiToRuntimeEvent::AgentDeleteRequested { .. }
                            | UiToRuntimeEvent::AgentGenerateRequested { .. } => {
                                let _ = event_tx
                                    .send(RuntimeToUiEvent::Error(
                                        "Cannot handle this request while a run is active".to_string(),
                                    ))
                                    .await;
                            }
                        }
                    }
                    else => break,
                }
            }
        }

        // 等待事件处理器自然退出（engine_tx drop 后 engine_rx 收到 None）
        let _ = processor.await;

        // 更新 session 消息计数（所有消息已被处理器增量持久化）
        let total = self.messages.len() as i64;
        db::global_db()
            .update_session_msg_count(self.session_id.as_ref().unwrap(), total)
            .await
            .expect("failed to update session message count");

        self.cancelled.store(false, Ordering::Relaxed);
        self.send_event(RuntimeToUiEvent::RunFinished).await;
    }

    /// 启动事件处理器
    async fn spawn_event_processor(
        &self,
        mut engine_rx: mpsc::Receiver<EngineToRuntimeEvent>,
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
        let project = self.project.clone();
        let blocks_dir = session_dir.path().join("blocks");

        tokio::spawn(async move {
            while let Some(event) = engine_rx.recv().await {
                match event {
                    // ===== 需要持久化的事件 =====
                    EngineToRuntimeEvent::UserMessageProduced(msg) => {
                        history::persist_one(&session_dir, &session_id, &blocks_dir, msg).await;
                    }
                    EngineToRuntimeEvent::MessageProduced(msg)
                    | EngineToRuntimeEvent::ToolResultsProduced(msg) => {
                        history::persist_one(&session_dir, &session_id, &blocks_dir, msg).await;
                    }
                    // ===== 透传事件 =====
                    EngineToRuntimeEvent::TurnStarted => {
                        let _ = event_tx.send(RuntimeToUiEvent::TurnStarted).await;
                    }
                    EngineToRuntimeEvent::TurnEnded => {
                        let _ = event_tx.send(RuntimeToUiEvent::TurnEnded).await;
                    }
                    EngineToRuntimeEvent::ThinkingDelta(t) => {
                        let _ = event_tx.send(RuntimeToUiEvent::ThinkingDelta(t)).await;
                    }
                    EngineToRuntimeEvent::TextDelta(t) => {
                        let _ = event_tx.send(RuntimeToUiEvent::TextDelta(t)).await;
                    }
                    EngineToRuntimeEvent::ToolUse(tu) => {
                        let _ = event_tx.send(RuntimeToUiEvent::ToolUse(tu)).await;
                    }
                    EngineToRuntimeEvent::ToolResult(tr) => {
                        let _ = event_tx.send(RuntimeToUiEvent::ToolResult(tr)).await;
                    }
                    EngineToRuntimeEvent::ToolPauseRequested(req) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::ToolPauseRequested(req))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentStarted(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::SubagentStarted(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentMessageProduced(event) => {
                        let parent_dir = project.session(&session_id);
                        let subagent_dir = parent_dir.subagent(&event.session_id);
                        let subagent_blocks_dir = subagent_dir.path().join("blocks");
                        history::persist_db_only(
                            &event.session_id,
                            &subagent_blocks_dir,
                            &event.message,
                        )
                        .await;
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
                        let _ = event_tx.send(RuntimeToUiEvent::Error(e)).await;
                    }
                    EngineToRuntimeEvent::Warning(warning) => {
                        let _ = event_tx.send(RuntimeToUiEvent::Warning(warning)).await;
                    }
                }
            }
        })
    }

    /// 发送事件到 UI（忽略 send 失败）
    pub(crate) async fn send_event(&self, event: RuntimeToUiEvent) {
        let _ = self.event_tx.send(event).await;
    }

    /// 首次提交时创建 session：生成 UUID、建目录、写 DB。
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
        // 从第一条用户消息中提取标题
        let title_text =
            history::title_text(initial_display_message.as_ref(), self.messages.last());
        let title = title_text.map(|text| text.chars().take(300).collect());
        let session = crate::db::Session {
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
            message_count: 0,
            created_at: now,
            updated_at: now,
        };
        db::global_db()
            .create_session(&session)
            .await
            .expect("failed to persist session");

        let title_out = session.title.clone();
        let session_id_out = session.id.clone();
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
        })
        .await;
    }
}
