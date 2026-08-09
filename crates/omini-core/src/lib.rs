pub mod engine;
pub mod error;
pub mod frontmatter;
pub mod mcp;
pub mod prompts;
pub mod runtime;
mod skills;
mod subagents;
pub mod title_generation;
pub mod tools;
pub mod types;
pub mod util;

use crate::runtime::AgentRuntime;
use omini_config::Settings;
use omini_config::project::ProjectDir;
use omini_domain::config::ProviderInfo;
use omini_domain::events::{ActiveProfile, SessionUsageSnapshot};
use omini_domain::message::Message;
use omini_domain::subagents as subagent_types;
use omini_runtime_contract::project as project_types;
use omini_runtime_contract::thread as thread_types;
use omini_runtime_contract::{RuntimePersistenceEvent, RuntimeToServerEvent, ServerToRuntimeEvent};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::Instrument;

pub use crate::error::CoreError;
pub use crate::title_generation::{TitleGenError, generate_thread_title};
pub use omini_domain::title_generation::GeneratedSessionTitle;

pub fn project_agents_snapshot(settings: &Settings) -> thread_types::AgentsSnapshot {
    let records = project_agent_records(&settings.cwd);
    let models = models_snapshot_from_settings(settings);
    thread_types::AgentsSnapshot {
        records,
        providers: models.providers,
        current_provider: models.current_provider,
        current_model: models.current_model,
    }
}

pub fn project_skill_summaries(cwd: &Path) -> Vec<thread_types::SkillSummarySnapshot> {
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
    settings: &Settings,
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

/// Thread-scoped core facade between `omini-server` and the agent runtime.
///
/// `omini-server::thread::ThreadRuntime` 为每个 daemon thread 持有一个
/// `AgentCoreThread`。server 通过这里把已经通过 HTTP/controller 校验的用户动作
/// 转成 `omini-runtim-contract` 的 `ServerToRuntimeEvent`，同时订阅 runtime 事件和持久化事件，再负责
/// SQLite 落盘、WebSocket fanout、replay buffer、presence 和 controller 语义。
///
/// runtime 输出保持 `omini-runtim-contract` 的 `RuntimeToServerEvent` 形态，协议层
/// `RuntimeEvent` 编码由 `omini-server` 的 runtime adapter 负责。
///
/// 边界约束：这里只桥接 core runtime 输入、runtime 输出、持久化输出和只读能力查询。
/// session registry、HTTP 状态码、WebSocket 订阅、controller 冲突、replay 以及数据库写入
/// 都属于 `omini-server`；不要把 daemon 级编排继续塞回 core。
pub struct AgentCoreThread {
    thread_id: String,
    // server 接受并鉴权后的用户动作从这里进入 core runtime。
    request_tx: mpsc::Sender<ServerToRuntimeEvent>,
    // runtime 输出事件按 server-core 契约广播给 server。
    event_tx: broadcast::Sender<RuntimeToServerEvent>,
    persistence_rx: Mutex<Option<mpsc::Receiver<RuntimePersistenceEvent>>>,
    // HTTP 查询和配置 mutation 需要读取当前会话配置快照；真正执行仍通过 request_tx 进入 runtime。
    settings: Arc<RwLock<Settings>>,
    // 与 runtime 共享同一个 MCP manager，保证 server 查询到的是当前会话实际运行状态。
    mcp_manager: Arc<crate::mcp::McpManager>,
    // 与 runtime 共享同一个能力 store，保证只读状态反映当前 session 实际能力。
    capabilities: Arc<crate::runtime::CapabilityStore>,
    // agent runtime 主循环：消费 request_tx 输入并驱动模型、工具、权限和内部事件。
    _runtime_handle: JoinHandle<()>,
    // runtime 事件 fanout：把 RuntimeToServerEvent 广播给 server。
    _fanout_handle: JoinHandle<()>,
}

pub struct AgentCoreThreadLoad {
    pub messages: Vec<Message>,
    pub llm_context_version: i64,
    pub usage: SessionUsageSnapshot,
    pub agent_tasks: Vec<omini_domain::events::AgentTaskInfo>,
}

impl AgentCoreThread {
    /// 启动一个 core runtime，并创建 server 可订阅的 runtime/persistence fanout。
    ///
    /// 唯一的生产入口 `spawn_for_session_with_active_profile` 强制要求传入一个
    /// 由 server 端已经预创建好的 `thread_id`（对应目录、DB 行都已存在），
    /// 并把持久化层的 messages / usage 一次性灌进 runtime,启动后 core 即处于
    /// "已加载"状态 —— 不再需要后续的 hydrate 事件。
    pub fn spawn_for_thread_with_active_profile(
        settings: Settings,
        project: ProjectDir,
        thread_id: String,
        active_profile: ActiveProfile,
        load: AgentCoreThreadLoad,
    ) -> Result<Self, CoreError> {
        let AgentCoreThreadLoad {
            messages,
            llm_context_version,
            usage,
            agent_tasks,
        } = load;
        let settings_snapshot = Arc::new(RwLock::new(settings.clone()));
        let (runtime_event_tx, mut runtime_event_rx) = mpsc::channel::<RuntimeToServerEvent>(512);
        let (runtime_persistence_tx, runtime_persistence_rx) =
            mpsc::channel::<RuntimePersistenceEvent>(512);
        let (request_tx, request_rx) = mpsc::channel::<ServerToRuntimeEvent>(512);
        let (event_tx, _) = broadcast::channel::<RuntimeToServerEvent>(512);
        let handles = crate::runtime::RuntimeCapabilityHandles::load(&settings);
        let mcp_manager = Arc::clone(&handles.mcp_manager);
        let capabilities = Arc::clone(&handles.capabilities);

        let thread_dir = project.thread(&thread_id);
        let channels = crate::runtime::AgentRuntimeChannels {
            event_tx: runtime_event_tx,
            persistence_tx: runtime_persistence_tx,
            request_rx,
        };
        let deps = crate::runtime::AgentRuntimeDeps {
            settings,
            project,
            thread_id: thread_id.clone(),
            thread_dir,
            messages,
            llm_context_version,
            usage,
            active_profile,
            agent_tasks,
        };
        let runtime = AgentRuntime::with_capability_handles(channels, deps, handles);
        let runtime_handle = runtime.run();
        let fanout_tx = event_tx.clone();
        let runtime_fanout_thread_id = thread_id.clone();
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
                thread_id = %runtime_fanout_thread_id,
                task_kind = "runtime_event_fanout"
            )),
        );

        Ok(Self {
            thread_id,
            request_tx,
            event_tx,
            persistence_rx: Mutex::new(Some(runtime_persistence_rx)),
            settings: settings_snapshot,
            mcp_manager,
            capabilities,
            _runtime_handle: runtime_handle,
            _fanout_handle: fanout_handle,
        })
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
    pub fn take_persistence_receiver(&self) -> Option<mpsc::Receiver<RuntimePersistenceEvent>> {
        self.persistence_rx
            .lock()
            .expect("persistence receiver lock poisoned")
            .take()
    }

    pub fn list_models(&self) -> thread_types::ModelsSnapshot {
        let settings = self.settings.read().expect("core settings lock poisoned");
        models_snapshot_from_settings(&settings)
    }

    pub fn list_agents(&self) -> thread_types::AgentsSnapshot {
        let settings = self.settings.read().expect("core settings lock poisoned");
        project_agents_snapshot(&settings)
    }

    pub fn list_skills(&self) -> Vec<thread_types::SkillSummarySnapshot> {
        let settings = self.settings.read().expect("core settings lock poisoned");
        user_invocable_skill_summaries(&settings.cwd)
    }

    pub fn get_skill(&self, skill_name: &str) -> Option<thread_types::SkillDetailSnapshot> {
        let settings = self.settings.read().expect("core settings lock poisoned");
        skill_detail_snapshot(&settings.cwd, skill_name)
    }

    pub fn runtime_skills(&self) -> Vec<thread_types::RuntimeSkillSnapshot> {
        let skill_registry = self.capabilities.skill_registry();
        let mut skills = skill_registry
            .skills()
            .map(|skill| thread_types::RuntimeSkillSnapshot {
                name: skill.name.clone(),
                description: skill.description.clone(),
                short_description: skill.short_description.clone(),
                source_kind: runtime_skill_source_kind(skill.source_kind()),
                directory: skill.directory.clone(),
                status: thread_types::RuntimeCapabilityStatus::Available,
                disable_model_invocation: skill.disable_model_invocation,
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

    pub fn runtime_mcp_servers(
        &self,
    ) -> Vec<omini_runtime_contract::mcp::RuntimeMcpServerSnapshot> {
        self.mcp_manager.runtime_snapshots()
    }

    pub fn runtime_subagents(&self) -> Vec<subagent_types::AgentSummary> {
        self.capabilities.subagent_registry().summaries()
    }

    pub async fn submit_run(
        &self,
        command: thread_types::SubmitRunCommand,
    ) -> Result<thread_types::RunSubmitted, CoreError> {
        let thread_types::SubmitRunCommand {
            draft,
            client_echo_id,
            mode,
        } = command;
        let event = match mode {
            thread_types::RunInputMode::Submit => ServerToRuntimeEvent::SendMessage {
                draft,
                client_echo_id,
            },
            thread_types::RunInputMode::Intervene => ServerToRuntimeEvent::InterveneMessage {
                draft,
                client_echo_id,
            },
        };
        self.send_to_runtime(event).await?;
        Ok(thread_types::RunSubmitted {
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
        command: thread_types::SetActiveProfileCommand,
    ) -> Result<(), CoreError> {
        self.send_to_runtime(ServerToRuntimeEvent::SetActiveProfile(command.profile))
            .await
    }

    pub async fn set_model(&self, command: thread_types::SetModelCommand) -> Result<(), CoreError> {
        let thread_types::SetModelCommand {
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
        command: thread_types::SetThinkingEffortCommand,
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
        command: thread_types::ResolveToolPauseCommand,
    ) -> Result<(), CoreError> {
        let thread_types::ResolveToolPauseCommand {
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
        command: thread_types::ResolvePlanCommand,
    ) -> Result<(), CoreError> {
        let thread_types::ResolvePlanCommand { plan_id, action } = command;
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
            thread_id = %self.thread_id,
            event_kind = server_to_runtime_event_kind(&event),
            "sending event to runtime"
        );
        self.request_tx
            .send(event)
            .await
            .map_err(|_| CoreError::RuntimeClosed)
    }
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

fn user_invocable_skill_summaries(cwd: &Path) -> Vec<thread_types::SkillSummarySnapshot> {
    let mut skills = crate::skills::load_skill_registry(cwd)
        .skills()
        .filter(|skill| skill.user_invocable)
        .map(|skill| thread_types::SkillSummarySnapshot {
            name: skill.name.clone(),
            description: skill.description.clone(),
            short_description: skill.short_description.clone(),
        })
        .collect::<Vec<_>>();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn skill_detail_snapshot(
    cwd: &Path,
    skill_name: &str,
) -> Option<thread_types::SkillDetailSnapshot> {
    let registry = crate::skills::load_skill_registry(cwd);
    registry
        .get(skill_name)
        .map(|skill| thread_types::SkillDetailSnapshot {
            name: skill.name.clone(),
            description: skill.description.clone(),
            short_description: skill.short_description.clone(),
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
        ServerToRuntimeEvent::ResolveToolPause { .. } => "resolve_tool_pause",
        ServerToRuntimeEvent::ResolvePlanApproval { .. } => "resolve_plan_approval",
        ServerToRuntimeEvent::SubagentRegistryChanged => "subagent_registry_changed",
        ServerToRuntimeEvent::CloseRuntime => "close_runtime",
    }
}

fn runtime_skill_source_kind(
    source_kind: crate::skills::SkillSourceKind,
) -> thread_types::RuntimeSkillSourceKind {
    match source_kind {
        crate::skills::SkillSourceKind::BuiltIn => thread_types::RuntimeSkillSourceKind::BuiltIn,
        crate::skills::SkillSourceKind::Project => thread_types::RuntimeSkillSourceKind::Project,
        crate::skills::SkillSourceKind::User => thread_types::RuntimeSkillSourceKind::User,
    }
}

fn runtime_skill_source_sort(source_kind: thread_types::RuntimeSkillSourceKind) -> u8 {
    match source_kind {
        thread_types::RuntimeSkillSourceKind::BuiltIn => 0,
        thread_types::RuntimeSkillSourceKind::Project => 1,
        thread_types::RuntimeSkillSourceKind::User => 2,
    }
}

fn models_snapshot_from_settings(settings: &Settings) -> thread_types::ModelsSnapshot {
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
    thread_types::ModelsSnapshot {
        providers,
        current_provider: settings.active_provider.clone(),
        current_model: settings.model.clone(),
    }
}
