pub mod config;
pub mod engine;
pub mod error;
pub mod frontmatter;
pub mod mcp;
pub mod permissions;
pub mod prompts;
pub mod runtime;
mod skills;
mod subagents;
pub mod tools;
pub mod types;

use crate::runtime::AgentRuntime;
use omini_domain::config::ProviderInfo;
use omini_domain::events::{ActiveProfile, LoadedSession};
use omini_domain::subagents as subagent_types;
use omini_domain::usage::Usage;
use omini_runtime_api::project as project_types;
use omini_runtime_api::session as session_types;
use omini_runtime_api::{RuntimePersistenceEvent, RuntimeToServerEvent, ServerToRuntimeEvent};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::Instrument;

pub use crate::error::CoreError;

pub fn project_agents_snapshot(
    settings: &crate::types::config::Settings,
) -> session_types::AgentsSnapshot {
    let records = project_agent_records(&settings.cwd);
    let models = models_snapshot_from_settings(settings);
    session_types::AgentsSnapshot {
        records,
        providers: models.providers,
        current_provider: models.current_provider,
        current_model: models.current_model,
    }
}

pub fn project_skill_summaries(cwd: &Path) -> Vec<session_types::SkillSummarySnapshot> {
    user_invocable_skill_summaries(cwd)
}

pub fn save_project_agent(
    cwd: &Path,
    command: project_types::SaveProjectAgentCommand,
) -> Result<project_types::AgentManagementUpdate, CoreError> {
    if command.source_kind == subagent_types::AgentSourceKind::BuiltIn {
        return Err(CoreError::new("内置 agent 不能写入"));
    }
    let original_path = command
        .original_agent_id
        .as_deref()
        .map(|agent_id| resolve_editable_agent_path(cwd, agent_id))
        .transpose()?;
    if crate::subagents::agent_name_exists(cwd, &command.draft.name, original_path.as_deref()) {
        return Err(CoreError::new(format!(
            "agent '{}' 已存在",
            command.draft.name
        )));
    }
    let written_path = crate::subagents::write_agent_file(cwd, command.source_kind, &command.draft)
        .map_err(CoreError::new)?;
    if let Some(path) = original_path
        && path != written_path
    {
        crate::subagents::delete_agent_file(&path).map_err(CoreError::new)?;
    }
    Ok(project_types::AgentManagementUpdate {
        records: project_agent_records(cwd),
    })
}

pub fn delete_project_agent(
    cwd: &Path,
    command: project_types::DeleteProjectAgentCommand,
) -> Result<project_types::AgentManagementUpdate, CoreError> {
    let path = resolve_editable_agent_path(cwd, &command.agent_id)?;
    crate::subagents::delete_agent_file(&path).map_err(CoreError::new)?;
    Ok(project_types::AgentManagementUpdate {
        records: project_agent_records(cwd),
    })
}

pub async fn generate_project_agent_draft(
    settings: &crate::types::config::Settings,
    description: &str,
) -> Result<subagent_types::GeneratedAgentDraft, CoreError> {
    let mut parse_error = None;
    for attempt in 0..2 {
        match crate::subagents::generate_agent_draft_checked_from_settings(settings, description)
            .await
        {
            Ok(draft) => return Ok(draft),
            Err(crate::subagents::GenerateAgentDraftError::Parse(message)) if attempt == 0 => {
                parse_error = Some(message);
            }
            Err(error) => return Err(CoreError::new(error.to_string())),
        }
    }
    Err(CoreError::new(
        parse_error.unwrap_or_else(|| "生成 agent 失败".to_string()),
    ))
}

/// 会话级 core facade，是 `omini-server` 和真正 agent runtime 之间的通信边界。
///
/// `omini-server::runtime::RuntimeSession` 为每个 daemon 会话持有一个
/// `AgentCoreSession`。server 通过这里把已经通过 HTTP/controller 校验的用户动作
/// 转成 `omini-runtime-api` 的 `ServerToRuntimeEvent`，同时订阅 runtime 事件和持久化事件，再负责
/// SQLite 落盘、WebSocket fanout、replay buffer、presence 和 controller 语义。
///
/// runtime 输出保持 `omini-runtime-api` 的 `RuntimeToServerEvent` 形态，协议层
/// `RuntimeEvent` 编码由 `omini-server` 的 runtime adapter 负责。
///
/// 边界约束：这里只桥接 core runtime 输入、runtime 输出、持久化输出和只读能力查询。
/// session registry、HTTP 状态码、WebSocket 订阅、controller 冲突、replay 以及数据库写入
/// 都属于 `omini-server`；不要把 daemon 级编排继续塞回 core。
pub struct AgentCoreSession {
    // server 持有的 daemon session id；core facade task 日志用它做稳定关联字段。
    session_id: Option<String>,
    // server 接受并鉴权后的用户动作从这里进入 core runtime。
    request_tx: mpsc::Sender<ServerToRuntimeEvent>,
    // runtime 输出事件按 server-core 契约广播给 server。
    event_tx: broadcast::Sender<RuntimeToServerEvent>,
    // 持久化事件按 server-core 契约广播给 server，由 server 负责事务、replay 裁剪和错误处理。
    persistence_tx: broadcast::Sender<RuntimePersistenceEvent>,
    // HTTP 查询和配置 mutation 需要读取当前会话配置快照；真正执行仍通过 request_tx 进入 runtime。
    settings: Arc<RwLock<crate::types::config::Settings>>,
    // 与 runtime 共享同一个 MCP manager，保证 server 查询到的是当前会话实际运行状态。
    mcp_manager: Arc<crate::mcp::McpManager>,
    // 与 runtime 共享同一个能力 store，保证只读状态反映当前 session 实际能力。
    capabilities: Arc<crate::runtime::CapabilityStore>,
    // agent runtime 主循环：消费 request_tx 输入并驱动模型、工具、权限和内部事件。
    _runtime_handle: JoinHandle<()>,
    // runtime 事件 fanout：把 RuntimeToServerEvent 广播给 server。
    _fanout_handle: JoinHandle<()>,
    // 持久化事件 fanout：把 core 产生的 RuntimePersistenceEvent 广播给 server 落盘。
    _persistence_handle: JoinHandle<()>,
}

impl AgentCoreSession {
    /// 启动一个 core runtime，并创建 server 可订阅的 runtime/persistence fanout。
    ///
    /// 返回值是 server 唯一需要持有的 core 会话句柄。runtime 本身只消费
    /// `ServerToRuntimeEvent`，这里额外启动 fanout 任务把 runtime 输出转成 broadcast
    /// stream，并把持久化事件转成 broadcast stream。
    pub fn spawn(
        settings: crate::types::config::Settings,
        project: config::project::ProjectDir,
    ) -> Self {
        Self::spawn_with_active_profile(settings, project, ActiveProfile::Main)
    }

    pub fn spawn_with_active_profile(
        settings: crate::types::config::Settings,
        project: config::project::ProjectDir,
        active_profile: ActiveProfile,
    ) -> Self {
        Self::spawn_with_session_id(settings, project, None, active_profile)
    }

    pub fn spawn_for_session_with_active_profile(
        settings: crate::types::config::Settings,
        project: config::project::ProjectDir,
        session_id: String,
        active_profile: ActiveProfile,
    ) -> Self {
        Self::spawn_with_session_id(settings, project, Some(session_id), active_profile)
    }

    fn spawn_with_session_id(
        settings: crate::types::config::Settings,
        project: config::project::ProjectDir,
        session_id: Option<String>,
        active_profile: ActiveProfile,
    ) -> Self {
        let settings_snapshot = Arc::new(RwLock::new(settings.clone()));
        let (runtime_event_tx, mut runtime_event_rx) = mpsc::channel::<RuntimeToServerEvent>(512);
        let (runtime_persistence_tx, mut runtime_persistence_rx) =
            mpsc::channel::<RuntimePersistenceEvent>(512);
        let (request_tx, request_rx) = mpsc::channel::<ServerToRuntimeEvent>(512);
        let (event_tx, _) = broadcast::channel::<RuntimeToServerEvent>(512);
        let (persistence_tx, _) = broadcast::channel::<RuntimePersistenceEvent>(512);
        let handles = crate::runtime::RuntimeCapabilityHandles::load(&settings);
        let mcp_manager = Arc::clone(&handles.mcp_manager);
        let capabilities = Arc::clone(&handles.capabilities);

        let runtime = AgentRuntime::with_capability_handles(
            runtime_event_tx,
            runtime_persistence_tx,
            request_rx,
            settings,
            project,
            handles,
            active_profile,
        );
        let runtime_handle = runtime.run();
        let fanout_tx = event_tx.clone();
        let runtime_fanout_session_id = session_id
            .clone()
            .unwrap_or_else(|| "unassigned".to_string());
        let fanout_handle = tokio::spawn(
            async move {
                tracing::debug!("core runtime event fanout started");
                while let Some(event) = runtime_event_rx.recv().await {
                    let _ = fanout_tx.send(event);
                }
                tracing::debug!("core runtime event fanout stopped");
            }
            .instrument(tracing::debug_span!(
                "core_fanout",
                session_id = %runtime_fanout_session_id,
                task_kind = "runtime_event_fanout"
            )),
        );
        let persistence_fanout_tx = persistence_tx.clone();
        let persistence_fanout_session_id = session_id
            .clone()
            .unwrap_or_else(|| "unassigned".to_string());
        let persistence_handle = tokio::spawn(
            async move {
                tracing::debug!("core persistence fanout started");
                while let Some(event) = runtime_persistence_rx.recv().await {
                    let summary = runtime_persistence_event_summary(&event);
                    tracing::trace!(
                        event_kind = summary.kind,
                        session_id = ?summary.session_id,
                        item_count = ?summary.item_count,
                        prompt_tokens = ?summary.prompt_tokens,
                        completion_tokens = ?summary.completion_tokens,
                        cached_tokens = ?summary.cached_tokens,
                        "fanout persistence event"
                    );
                    let _ = persistence_fanout_tx.send(event);
                }
                tracing::debug!("core persistence fanout stopped");
            }
            .instrument(tracing::debug_span!(
                "core_fanout",
                session_id = %persistence_fanout_session_id,
                task_kind = "persistence_fanout"
            )),
        );

        Self {
            session_id,
            request_tx,
            event_tx,
            persistence_tx,
            settings: settings_snapshot,
            mcp_manager,
            capabilities,
            _runtime_handle: runtime_handle,
            _fanout_handle: fanout_handle,
            _persistence_handle: persistence_handle,
        }
    }

    /// 订阅 core 内部 runtime 事件流。
    ///
    /// 每个 subscriber 都会收到 `RuntimeToServerEvent`，server 会在此基础上编码协议事件、
    /// 追加本地序号、维护 replay/status projection，并通过 WebSocket 发给控制者和观察者。
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeToServerEvent> {
        self.event_tx.subscribe()
    }

    /// 订阅 core 产生的持久化事件。
    ///
    /// core 不直接写 daemon 数据库；server 消费这个 stream 后负责落盘和 replay buffer 裁剪。
    pub fn subscribe_persistence(&self) -> broadcast::Receiver<RuntimePersistenceEvent> {
        self.persistence_tx.subscribe()
    }

    pub fn list_models(&self) -> session_types::ModelsSnapshot {
        let settings = self.settings.read().expect("core settings lock poisoned");
        models_snapshot_from_settings(&settings)
    }

    pub async fn load_session(&self, snapshot: LoadedSession) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::HydrateSessionSnapshot { snapshot })
            .await
    }

    pub fn list_agents(&self) -> session_types::AgentsSnapshot {
        let settings = self.settings.read().expect("core settings lock poisoned");
        project_agents_snapshot(&settings)
    }

    pub fn list_skills(&self) -> Vec<session_types::SkillSummarySnapshot> {
        let settings = self.settings.read().expect("core settings lock poisoned");
        user_invocable_skill_summaries(&settings.cwd)
    }

    pub fn get_skill(&self, skill_name: &str) -> Option<session_types::SkillDetailSnapshot> {
        let settings = self.settings.read().expect("core settings lock poisoned");
        skill_detail_snapshot(&settings.cwd, skill_name)
    }

    pub fn runtime_skills(&self) -> Vec<session_types::RuntimeSkillSnapshot> {
        let skill_registry = self.capabilities.skill_registry();
        let mut skills = skill_registry
            .skills()
            .map(|skill| session_types::RuntimeSkillSnapshot {
                name: skill.name.clone(),
                description: skill.description.clone(),
                source_kind: runtime_skill_source_kind(skill.source_kind()),
                directory: skill.directory.clone(),
                status: session_types::RuntimeCapabilityStatus::Available,
                inject: skill.inject,
                user_invocable: skill.user_invocable,
            })
            .collect::<Vec<_>>();
        skills.sort_by(|left, right| {
            runtime_skill_source_sort(left.source_kind)
                .cmp(&runtime_skill_source_sort(right.source_kind))
                .then_with(|| left.name.cmp(&right.name))
        });
        skills
    }

    pub fn runtime_mcp_servers(&self) -> Vec<omini_runtime_api::mcp::RuntimeMcpServerSnapshot> {
        self.mcp_manager.runtime_snapshots()
    }

    pub fn runtime_subagents(&self) -> Vec<subagent_types::AgentSummary> {
        self.capabilities.subagent_registry().summaries()
    }

    pub async fn submit_run(
        &self,
        command: session_types::SubmitRunCommand,
    ) -> Result<session_types::RunSubmitted, CoreError> {
        let session_types::SubmitRunCommand {
            draft,
            client_echo_id,
            mode,
        } = command;
        let event = match mode {
            session_types::RunInputMode::Submit => ServerToRuntimeEvent::SendMessage {
                draft,
                client_echo_id,
            },
            session_types::RunInputMode::Intervene => ServerToRuntimeEvent::InterveneMessage {
                draft,
                client_echo_id,
            },
        };
        self.send_to_runtime(event).await?;
        Ok(session_types::RunSubmitted {
            run_id: "current".to_string(),
        })
    }

    pub async fn cancel_run(&self) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::CancelRun).await
    }

    pub async fn compact_context(&self, instructions: Option<String>) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::CompactContext { instructions })
            .await
    }

    pub async fn toggle_active_profile(&self) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::ToggleActiveProfile)
            .await
    }

    pub async fn set_active_profile(
        &self,
        command: session_types::SetActiveProfileCommand,
    ) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::SetActiveProfile(command.profile))
            .await
    }

    pub async fn set_model(
        &self,
        command: session_types::SetModelCommand,
    ) -> Result<(), CoreError> {
        let session_types::SetModelCommand {
            provider,
            model,
            thinking_effort: requested_effort,
        } = command;
        let thinking_effort;
        {
            let mut settings = self.settings.write().expect("core settings lock poisoned");
            thinking_effort =
                settings.effective_thinking_effort_for(&provider, &model, requested_effort);
            settings.active_provider = provider.clone();
            settings.model = model.clone();
            settings.thinking_effort = thinking_effort;
        }
        self.send_to_runtime(ServerToRuntimeEvent::ModelSelected {
            provider,
            model,
            thinking_effort,
        })
        .await
    }

    pub async fn set_thinking_effort(
        &self,
        command: session_types::SetThinkingEffortCommand,
    ) -> Result<(), CoreError> {
        let requested_effort = command.effort;
        {
            let mut settings = self.settings.write().expect("core settings lock poisoned");
            settings.thinking_effort =
                settings.effective_current_thinking_effort(Some(requested_effort));
        }
        self.send_to_runtime(ServerToRuntimeEvent::SetThinkingEffort(requested_effort))
            .await
    }

    pub async fn resolve_tool_pause(
        &self,
        command: session_types::ResolveToolPauseCommand,
    ) -> Result<(), CoreError> {
        let session_types::ResolveToolPauseCommand {
            tool_use_id,
            response,
        } = command;
        self.send_to_runtime(ServerToRuntimeEvent::ResolveToolPause {
            tool_use_id,
            response,
        })
        .await
    }

    pub async fn resolve_plan(
        &self,
        command: session_types::ResolvePlanCommand,
    ) -> Result<(), CoreError> {
        let session_types::ResolvePlanCommand { plan_id, action } = command;
        self.send_to_runtime(ServerToRuntimeEvent::ResolvePlanApproval { plan_id, action })
            .await
    }

    pub async fn reload_subagent_registry(&self) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::SubagentRegistryChanged)
            .await
    }

    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::CloseRuntime)
            .await
    }

    /// 向 runtime 投递一个已通过 server 校验的内部事件。
    ///
    /// channel 关闭意味着对应会话的 runtime 已退出，调用方应把它视为 core 会话不可用。
    async fn send_to_runtime(&self, event: ServerToRuntimeEvent) -> Result<(), CoreError> {
        tracing::trace!(
            session_id = %session_log_id(self.session_id.as_deref()),
            event_kind = server_to_runtime_event_kind(&event),
            "sending event to runtime"
        );
        self.request_tx
            .send(event)
            .await
            .map_err(|_| CoreError::RuntimeClosed)
    }
}

fn session_log_id(session_id: Option<&str>) -> &str {
    session_id.unwrap_or("unassigned")
}

fn project_agent_records(cwd: &Path) -> Vec<subagent_types::AgentRecord> {
    crate::subagents::list_agent_records(cwd)
}

fn resolve_editable_agent_path(cwd: &Path, agent_id: &str) -> Result<PathBuf, CoreError> {
    project_agent_records(cwd)
        .into_iter()
        .find(|record| record.editable && agent_record_id(record) == agent_id)
        .and_then(|record| record.path)
        .ok_or_else(|| CoreError::new(format!("agent '{agent_id}' 不存在或不可编辑")))
}

fn agent_record_id(record: &subagent_types::AgentRecord) -> String {
    record
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| record.name.clone())
}

fn user_invocable_skill_summaries(cwd: &Path) -> Vec<session_types::SkillSummarySnapshot> {
    let mut skills = crate::skills::load_skill_registry(cwd)
        .skills()
        .filter(|skill| skill.user_invocable)
        .map(|skill| session_types::SkillSummarySnapshot {
            name: skill.name.clone(),
            description: skill.description.clone(),
        })
        .collect::<Vec<_>>();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn skill_detail_snapshot(
    cwd: &Path,
    skill_name: &str,
) -> Option<session_types::SkillDetailSnapshot> {
    let registry = crate::skills::load_skill_registry(cwd);
    registry
        .get(skill_name)
        .map(|skill| session_types::SkillDetailSnapshot {
            name: skill.name.clone(),
            description: skill.description.clone(),
            body: skill.body.clone(),
            directory: skill.directory.clone(),
            user_invocable: skill.user_invocable,
        })
}

fn server_to_runtime_event_kind(event: &ServerToRuntimeEvent) -> &'static str {
    match event {
        ServerToRuntimeEvent::SendMessage { .. } => "send_message",
        ServerToRuntimeEvent::InterveneMessage { .. } => "intervene_message",
        ServerToRuntimeEvent::CancelRun => "cancel_run",
        ServerToRuntimeEvent::CompactContext { .. } => "compact_context",
        ServerToRuntimeEvent::ModelSelected { .. } => "model_selected",
        ServerToRuntimeEvent::SetThinkingEffort(_) => "set_thinking_effort",
        ServerToRuntimeEvent::ToggleActiveProfile => "toggle_active_profile",
        ServerToRuntimeEvent::SetActiveProfile(_) => "set_active_profile",
        ServerToRuntimeEvent::HydrateSessionSnapshot { .. } => "hydrate_session_snapshot",
        ServerToRuntimeEvent::ResolveToolPause { .. } => "resolve_tool_pause",
        ServerToRuntimeEvent::ResolvePlanApproval { .. } => "resolve_plan_approval",
        ServerToRuntimeEvent::SubagentRegistryChanged => "subagent_registry_changed",
        ServerToRuntimeEvent::CloseRuntime => "close_runtime",
    }
}

struct RuntimePersistenceEventSummary<'a> {
    kind: &'static str,
    session_id: Option<&'a str>,
    item_count: Option<usize>,
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
    cached_tokens: Option<usize>,
}

fn runtime_persistence_event_summary(
    event: &RuntimePersistenceEvent,
) -> RuntimePersistenceEventSummary<'_> {
    match event {
        RuntimePersistenceEvent::CreateSession(session) => RuntimePersistenceEventSummary {
            kind: "create_session",
            session_id: Some(&session.id),
            item_count: None,
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
        },
        RuntimePersistenceEvent::UpdateSessionUpdatedAt { session_id } => {
            RuntimePersistenceEventSummary {
                kind: "update_session_updated_at",
                session_id: Some(session_id),
                item_count: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
            }
        }
        RuntimePersistenceEvent::UpdateSessionConfig { session_id, .. } => {
            RuntimePersistenceEventSummary {
                kind: "update_session_config",
                session_id: Some(session_id),
                item_count: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
            }
        }
        RuntimePersistenceEvent::UpdateSessionThinkingEffort { session_id, .. } => {
            RuntimePersistenceEventSummary {
                kind: "update_session_thinking_effort",
                session_id: Some(session_id),
                item_count: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
            }
        }
        RuntimePersistenceEvent::InsertMessage {
            session_id, blocks, ..
        } => RuntimePersistenceEventSummary {
            kind: "insert_message",
            session_id: Some(session_id),
            item_count: Some(blocks.len()),
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
        },
        RuntimePersistenceEvent::InsertDisplayMessage { session_id, .. } => {
            RuntimePersistenceEventSummary {
                kind: "insert_display_message",
                session_id: Some(session_id),
                item_count: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
            }
        }
        RuntimePersistenceEvent::InsertPlanMessage { session_id, .. } => {
            RuntimePersistenceEventSummary {
                kind: "insert_plan_message",
                session_id: Some(session_id),
                item_count: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
            }
        }
        RuntimePersistenceEvent::InsertCompactSummaryMessage { session_id, .. } => {
            RuntimePersistenceEventSummary {
                kind: "insert_compact_summary_message",
                session_id: Some(session_id),
                item_count: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
            }
        }
        RuntimePersistenceEvent::RecordSessionUsage { session_id, usage } => {
            usage_persistence_summary("record_session_usage", session_id, *usage)
        }
        RuntimePersistenceEvent::RecordSessionTotalUsage { session_id, usage } => {
            usage_persistence_summary("record_session_total_usage", session_id, *usage)
        }
        RuntimePersistenceEvent::RecordParentSubagentUsage { session_id, usage } => {
            usage_persistence_summary("record_parent_subagent_usage", session_id, *usage)
        }
    }
}

fn usage_persistence_summary<'a>(
    kind: &'static str,
    session_id: &'a str,
    usage: Usage,
) -> RuntimePersistenceEventSummary<'a> {
    RuntimePersistenceEventSummary {
        kind,
        session_id: Some(session_id),
        item_count: None,
        prompt_tokens: Some(usage.prompt_tokens),
        completion_tokens: Some(usage.completion_tokens),
        cached_tokens: Some(usage.cached_tokens),
    }
}

fn runtime_skill_source_kind(
    source_kind: crate::skills::SkillSourceKind,
) -> session_types::RuntimeSkillSourceKind {
    match source_kind {
        crate::skills::SkillSourceKind::BuiltIn => session_types::RuntimeSkillSourceKind::BuiltIn,
        crate::skills::SkillSourceKind::Project => session_types::RuntimeSkillSourceKind::Project,
        crate::skills::SkillSourceKind::User => session_types::RuntimeSkillSourceKind::User,
    }
}

fn runtime_skill_source_sort(source_kind: session_types::RuntimeSkillSourceKind) -> u8 {
    match source_kind {
        session_types::RuntimeSkillSourceKind::BuiltIn => 0,
        session_types::RuntimeSkillSourceKind::Project => 1,
        session_types::RuntimeSkillSourceKind::User => 2,
    }
}

fn models_snapshot_from_settings(
    settings: &crate::types::config::Settings,
) -> session_types::ModelsSnapshot {
    let mut providers = settings
        .providers
        .iter()
        .map(|(id, provider)| ProviderInfo {
            id: id.clone(),
            name: provider.name.clone(),
            endpoint: provider.endpoint,
            base_url: provider.base_url.clone(),
            models: provider.models.clone(),
        })
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    session_types::ModelsSnapshot {
        providers,
        current_provider: settings.active_provider.clone(),
        current_model: settings.model.clone(),
    }
}
