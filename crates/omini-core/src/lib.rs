pub mod api;
pub mod config;
pub mod engine;
pub mod frontmatter;
pub mod mcp;
pub mod permissions;
pub mod persistence;
pub mod prompts;
pub mod runtime;
pub mod skills;
pub mod subagents;
pub mod tools;
pub mod types;
pub mod util;

use crate::runtime::AgentRuntime;
use crate::types::display as display_types;
use crate::types::events as event_types;
use crate::types::events::{RuntimeToUiEvent, UiToRuntimeEvent};
use crate::types::subagents as subagent_types;
use omini_protocol as protocol;
use omini_protocol::RuntimeEvent;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct CoreError {
    message: String,
}

impl CoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CoreError {}

/// 会话级 core facade，是 `omini-server` 和真正 agent runtime 之间的通信边界。
///
/// `omini-server::runtime::RuntimeSession` 为每个 daemon 会话持有一个
/// `AgentCoreSession`。server 通过这里把已经通过 HTTP/controller 校验的用户动作
/// 转成 core 内部的 `UiToRuntimeEvent`，同时订阅 runtime 事件和持久化事件，再负责
/// SQLite 落盘、WebSocket fanout、replay buffer、presence 和 controller 语义。
///
/// 这个类型目前还承担 core 到 protocol 的适配：内部 `RuntimeToUiEvent` 会被编码成
/// `omini_protocol::RuntimeEvent`，并尽量附带 typed overlay 供新客户端消费。新增事件时
/// 要同步考虑 `key_runtime_event_from_internal`，否则 server/TUI 只能看到 legacy payload。
///
/// 边界约束：这里只桥接 core runtime 输入、runtime 输出、持久化输出和只读能力查询。
/// session registry、HTTP 状态码、WebSocket 订阅、controller 冲突、replay 以及数据库写入
/// 都属于 `omini-server`；不要把 daemon 级编排继续塞回 core。
pub struct AgentCoreSession {
    // server 接受并鉴权后的用户动作从这里进入 core runtime。
    request_tx: mpsc::Sender<UiToRuntimeEvent>,
    // runtime UI 事件在 fanout 任务中转成协议事件后广播给 server。
    event_tx: broadcast::Sender<RuntimeEvent>,
    // 持久化事件保持 core 内部形态，由 server 负责事务、replay 裁剪和错误处理。
    persistence_tx: broadcast::Sender<crate::persistence::RuntimePersistenceEvent>,
    // HTTP 查询和配置 mutation 需要读取当前会话配置快照；真正执行仍通过 request_tx 进入 runtime。
    settings: Arc<RwLock<crate::types::config::Settings>>,
    // 与 runtime 共享同一个 MCP manager，保证 server 查询到的是当前会话实际运行状态。
    mcp_manager: Arc<crate::mcp::McpManager>,
    // agent runtime 主循环：消费 request_tx 输入并驱动模型、工具、权限和内部事件。
    _runtime_handle: JoinHandle<()>,
    // runtime 事件 fanout：把 RuntimeToUiEvent 编码成 RuntimeEvent 并广播给 server。
    _fanout_handle: JoinHandle<()>,
    // 持久化事件 fanout：把 core 产生的 RuntimePersistenceEvent 广播给 server 落盘。
    _persistence_handle: JoinHandle<()>,
}

impl AgentCoreSession {
    /// 启动一个 core runtime，并创建 server 可订阅的 runtime/persistence fanout。
    ///
    /// 返回值是 server 唯一需要持有的 core 会话句柄。runtime 本身只消费
    /// `UiToRuntimeEvent`，这里额外启动 fanout 任务把 runtime 输出转换成 server
    /// 能广播的协议事件，并把持久化事件转成 broadcast stream。
    pub fn spawn(
        settings: crate::types::config::Settings,
        project: config::project::ProjectDir,
    ) -> Self {
        let settings_snapshot = Arc::new(RwLock::new(settings.clone()));
        let (runtime_event_tx, mut runtime_event_rx) = mpsc::channel::<RuntimeToUiEvent>(512);
        let (runtime_persistence_tx, mut runtime_persistence_rx) =
            mpsc::channel::<crate::persistence::RuntimePersistenceEvent>(512);
        let (request_tx, request_rx) = mpsc::channel::<UiToRuntimeEvent>(512);
        let (event_tx, _) = broadcast::channel::<RuntimeEvent>(512);
        let (persistence_tx, _) =
            broadcast::channel::<crate::persistence::RuntimePersistenceEvent>(512);
        let mcp_manager = Arc::new(crate::mcp::McpManager::from_settings(&settings));

        let runtime = AgentRuntime::with_mcp_manager(
            runtime_event_tx,
            runtime_persistence_tx,
            request_rx,
            settings,
            project,
            Arc::clone(&mcp_manager),
        );
        let runtime_handle = runtime.run();
        let fanout_tx = event_tx.clone();
        let fanout_handle = tokio::spawn(async move {
            while let Some(event) = runtime_event_rx.recv().await {
                let key_event = key_runtime_event_from_internal(&event);
                match serde_json::to_value(event) {
                    Ok(payload) => {
                        let mut event =
                            protocol::RuntimeEvent::new(runtime_event_kind(&payload), payload);
                        if let Some(key_event) = key_event {
                            event = event.with_key_event(key_event);
                        }
                        let _ = fanout_tx.send(event);
                    }
                    Err(error) => {
                        let payload = serde_json::json!({
                            "type": "notification",
                            "kind": "error",
                            "message": format!("Failed to encode runtime event: {error}"),
                            "details": [],
                        });
                        let _ =
                            fanout_tx.send(protocol::RuntimeEvent::new("notification", payload));
                    }
                }
            }
        });
        let persistence_fanout_tx = persistence_tx.clone();
        let persistence_handle = tokio::spawn(async move {
            while let Some(event) = runtime_persistence_rx.recv().await {
                let _ = persistence_fanout_tx.send(event);
            }
        });

        Self {
            request_tx,
            event_tx,
            persistence_tx,
            settings: settings_snapshot,
            mcp_manager,
            _runtime_handle: runtime_handle,
            _fanout_handle: fanout_handle,
            _persistence_handle: persistence_handle,
        }
    }

    /// 订阅面向客户端的 runtime 事件流。
    ///
    /// 每个 subscriber 都会收到 `RuntimeEvent`，server 会在此基础上追加本地序号、
    /// 维护 replay/status projection，并通过 WebSocket 发给控制者和观察者。
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.event_tx.subscribe()
    }

    /// 订阅 core 产生的持久化事件。
    ///
    /// core 不直接写 daemon 数据库；server 消费这个 stream 后负责落盘和 replay buffer 裁剪。
    pub fn subscribe_persistence(
        &self,
    ) -> broadcast::Receiver<crate::persistence::RuntimePersistenceEvent> {
        self.persistence_tx.subscribe()
    }

    pub fn list_models(&self) -> protocol::ModelsResponse {
        let settings = self.settings.read().expect("core settings lock poisoned");
        models_response_from_settings(&settings)
    }

    pub async fn load_session(
        &self,
        snapshot: event_types::LoadedSession,
    ) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::SessionSelected { snapshot })
            .await
    }

    pub fn list_agents(&self) -> protocol::AgentsResponse {
        let settings = self.settings.read().expect("core settings lock poisoned");
        let records = crate::subagents::list_agent_records(&settings.cwd)
            .into_iter()
            .map(agent_record_to_protocol)
            .collect();
        let models = models_response_from_settings(&settings);
        protocol::AgentsResponse {
            records,
            providers: models.providers,
            current_provider: models.current_provider,
            current_model: models.current_model,
        }
    }

    pub fn list_skills(&self) -> protocol::SkillsResponse {
        let settings = self.settings.read().expect("core settings lock poisoned");
        let mut skills = crate::skills::load_skill_registry(&settings.cwd)
            .skills()
            .filter(|skill| skill.user_invocable)
            .map(|skill| protocol::SkillSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect::<Vec<_>>();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        protocol::SkillsResponse { skills }
    }

    pub fn get_skill(&self, skill_name: &str) -> Option<protocol::SkillResponse> {
        let settings = self.settings.read().expect("core settings lock poisoned");
        let registry = crate::skills::load_skill_registry(&settings.cwd);
        registry
            .get(skill_name)
            .map(|skill| protocol::SkillResponse {
                skill: protocol::SkillDetail {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    body: skill.body.clone(),
                    directory: skill.directory.display().to_string(),
                    user_invocable: skill.user_invocable,
                },
            })
    }

    pub fn runtime_skills(&self) -> Vec<protocol::SessionRuntimeSkill> {
        let settings = self.settings.read().expect("core settings lock poisoned");
        let mut skills = crate::skills::load_skill_registry(&settings.cwd)
            .skills()
            .map(|skill| protocol::SessionRuntimeSkill {
                name: skill.name.clone(),
                description: skill.description.clone(),
                source_kind: skill_source_kind_to_protocol(skill.source_kind()),
                directory: skill.directory.display().to_string(),
                status: protocol::SessionRuntimeCapabilityStatus::Available,
                inject: skill.inject,
                user_invocable: skill.user_invocable,
            })
            .collect::<Vec<_>>();
        skills.sort_by(|left, right| {
            skill_source_sort(left.source_kind)
                .cmp(&skill_source_sort(right.source_kind))
                .then_with(|| left.name.cmp(&right.name))
        });
        skills
    }

    pub fn runtime_mcp_servers(&self) -> Vec<protocol::SessionRuntimeMcpServer> {
        self.mcp_manager.protocol_status()
    }

    pub async fn submit_run(
        &self,
        request: protocol::SubmitRunRequest,
    ) -> Result<protocol::RunSubmittedResponse, CoreError> {
        let event = match request.mode {
            protocol::RunInputMode::Submit => {
                UiToRuntimeEvent::SendMessage(user_input_from_protocol(request.input))
            }
            protocol::RunInputMode::Intervene => {
                UiToRuntimeEvent::InterveneMessage(user_input_from_protocol(request.input))
            }
        };
        self.send_runtime_event(event).await?;
        Ok(protocol::RunSubmittedResponse {
            run_id: "current".to_string(),
        })
    }

    pub async fn cancel_run(&self) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::CancelRun).await
    }

    pub async fn new_session(&self) -> Result<protocol::CreateSessionResponse, CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::ClearSession)
            .await?;
        Ok(protocol::CreateSessionResponse { session_id: None })
    }

    pub async fn compact_context(&self, instructions: Option<String>) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::CompactContext { instructions })
            .await
    }

    pub async fn toggle_active_profile(&self) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::ToggleActiveProfile)
            .await
    }

    pub async fn set_active_profile(
        &self,
        request: protocol::SetActiveProfileRequest,
    ) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::SetActiveProfile(
            active_profile_from_protocol(request.profile),
        ))
        .await
    }

    pub async fn set_model(&self, request: protocol::SetModelRequest) -> Result<(), CoreError> {
        {
            let mut settings = self.settings.write().expect("core settings lock poisoned");
            settings.active_provider = request.provider.clone();
            settings.model = request.model.clone();
            settings.thinking_effort = request.thinking_effort.map(thinking_effort_from_protocol);
        }
        self.send_runtime_event(UiToRuntimeEvent::ModelSelected {
            provider: request.provider,
            model: request.model,
            thinking_effort: request.thinking_effort.map(thinking_effort_from_protocol),
        })
        .await
    }

    pub async fn set_thinking_effort(
        &self,
        request: protocol::SetThinkingEffortRequest,
    ) -> Result<(), CoreError> {
        {
            let mut settings = self.settings.write().expect("core settings lock poisoned");
            settings.thinking_effort = Some(thinking_effort_from_protocol(request.effort));
        }
        self.send_runtime_event(UiToRuntimeEvent::SetThinkingEffort(
            thinking_effort_from_protocol(request.effort),
        ))
        .await
    }

    pub async fn set_thinking_display(
        &self,
        request: protocol::SetThinkingDisplayRequest,
    ) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::SetThinkingDisplay { show: request.show })
            .await
    }

    pub async fn resolve_tool_pause(
        &self,
        tool_use_id: String,
        request: protocol::ResolveToolPauseRequest,
    ) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::ResolveToolPause {
            tool_use_id,
            response: tool_pause_response_from_protocol(request.response),
        })
        .await
    }

    pub async fn resolve_plan(
        &self,
        plan_id: String,
        request: protocol::ResolvePlanRequest,
    ) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::ResolvePlanApproval {
            plan_id,
            action: plan_approval_action_from_protocol(request.action),
        })
        .await
    }

    pub async fn save_agent(&self, request: protocol::SaveAgentRequest) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::AgentSaveRequested {
            source_kind: agent_source_kind_from_protocol(request.source_kind),
            original_path: request.original_agent_id.map(PathBuf::from),
            draft: agent_draft_from_protocol(request.draft),
        })
        .await
    }

    pub async fn delete_agent(&self, agent_id: String) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::AgentDeleteRequested {
            path: PathBuf::from(agent_id),
        })
        .await
    }

    pub async fn generate_agent(
        &self,
        request: protocol::GenerateAgentRequest,
    ) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::AgentGenerateRequested {
            source_kind: agent_source_kind_from_protocol(request.source_kind),
            description: request.description,
            tools: request.tools,
            disallow_tools: request.disallow_tools,
            model: request.model,
        })
        .await
    }

    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.send_runtime_event(UiToRuntimeEvent::ShutdownRequested)
            .await
    }

    /// 向 runtime 投递一个已通过 server 校验的内部事件。
    ///
    /// channel 关闭意味着对应会话的 runtime 已退出，调用方应把它视为 core 会话不可用。
    async fn send_runtime_event(&self, event: UiToRuntimeEvent) -> Result<(), CoreError> {
        self.request_tx
            .send(event)
            .await
            .map_err(|_| CoreError::new("Runtime session is closed"))
    }
}

fn runtime_event_kind(payload: &serde_json::Value) -> String {
    payload
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("runtime.event")
        .to_string()
}

fn key_runtime_event_from_internal(
    event: &event_types::RuntimeToUiEvent,
) -> Option<protocol::KeyRuntimeEvent> {
    match event {
        event_types::RuntimeToUiEvent::RunStarted => Some(protocol::KeyRuntimeEvent::RunStarted),
        event_types::RuntimeToUiEvent::RunFinished => Some(protocol::KeyRuntimeEvent::RunFinished),
        event_types::RuntimeToUiEvent::Notification(notification) => Some(
            protocol::KeyRuntimeEvent::Notification(protocol::NotificationEvent {
                level: notification_level_to_protocol(notification.kind),
                message: notification.message.clone(),
                details: notification.details.clone(),
            }),
        ),
        event_types::RuntimeToUiEvent::ActiveProfileChanged(profile) => {
            Some(protocol::KeyRuntimeEvent::ActiveProfileChanged(
                protocol::ActiveProfileChangedEvent { profile: *profile },
            ))
        }
        event_types::RuntimeToUiEvent::ToolPauseRequested(request) => Some(
            protocol::KeyRuntimeEvent::ToolPauseRequested(protocol::ToolPauseRequestedEvent {
                tool_use_id: request.tool_use_id.clone(),
                tool_name: request.tool_name.clone(),
                kind: match &request.kind {
                    event_types::ToolPauseKind::Permission(_) => {
                        protocol::ToolPauseEventKind::Permission
                    }
                    event_types::ToolPauseKind::UserInput(_) => {
                        protocol::ToolPauseEventKind::UserInput
                    }
                },
                source_session_id: request.source_session_id.clone(),
                source_agent_label: request.source_agent_label.clone(),
            }),
        ),
        event_types::RuntimeToUiEvent::PlanSubmitted(plan) => Some(
            protocol::KeyRuntimeEvent::PlanSubmitted(protocol::PlanSubmittedEvent {
                plan_id: plan.id.clone(),
                title: plan.title.clone(),
                markdown: plan.markdown.clone(),
            }),
        ),
        event_types::RuntimeToUiEvent::PlanApprovalResolved { plan_id, action } => Some(
            protocol::KeyRuntimeEvent::PlanApprovalResolved(protocol::PlanApprovalResolvedEvent {
                plan_id: plan_id.clone(),
                action: *action,
            }),
        ),
        event_types::RuntimeToUiEvent::CompactSummaryStarted(event) => {
            Some(protocol::KeyRuntimeEvent::CompactSummaryStarted(
                protocol::CompactSummaryStartedEvent {
                    trigger: event.trigger,
                    session_id: event.session_id.clone(),
                    agent_label: event.agent_label.clone(),
                },
            ))
        }
        event_types::RuntimeToUiEvent::CompactSummaryDelta(event) => Some(
            protocol::KeyRuntimeEvent::CompactSummaryDelta(protocol::CompactSummaryDeltaEvent {
                trigger: event.trigger,
                delta: event.delta.clone(),
                session_id: event.session_id.clone(),
                agent_label: event.agent_label.clone(),
            }),
        ),
        event_types::RuntimeToUiEvent::CompactSummaryFinished(event) => {
            Some(protocol::KeyRuntimeEvent::CompactSummaryFinished(
                protocol::CompactSummaryFinishedEvent {
                    trigger: event.trigger,
                    summary: event.summary.clone(),
                    after_tokens: event.after_tokens,
                    session_id: event.session_id.clone(),
                    agent_label: event.agent_label.clone(),
                },
            ))
        }
        event_types::RuntimeToUiEvent::CompactSummaryFailed(event) => Some(
            protocol::KeyRuntimeEvent::CompactSummaryFailed(protocol::CompactSummaryFailedEvent {
                trigger: event.trigger,
                message: event.message.clone(),
                session_id: event.session_id.clone(),
                agent_label: event.agent_label.clone(),
            }),
        ),
        event_types::RuntimeToUiEvent::SessionChanged {
            session_id,
            messages,
            subagents,
            usage,
        } => Some(protocol::KeyRuntimeEvent::SessionSnapshot(
            protocol::SessionSnapshotEvent {
                session_id: session_id.clone(),
                message_count: messages.len(),
                subagent_count: subagents.len(),
                usage: protocol::SessionUsage {
                    current_context_tokens: usage.current_context_tokens,
                    total_tokens: usage.total_tokens,
                    total_cached_tokens: usage.total_cached_tokens,
                    context_window: usage.context_window,
                },
            },
        )),
        _ => None,
    }
}

fn notification_level_to_protocol(
    kind: event_types::NotificationKind,
) -> protocol::NotificationLevel {
    match kind {
        event_types::NotificationKind::Info => protocol::NotificationLevel::Info,
        event_types::NotificationKind::Warn => protocol::NotificationLevel::Warn,
        event_types::NotificationKind::Error => protocol::NotificationLevel::Error,
    }
}

fn user_input_from_protocol(input: protocol::UserInput) -> display_types::UserDraft {
    display_types::UserDraft {
        text: input.text.clone(),
        mentions: input
            .context_refs
            .unwrap_or_default()
            .into_iter()
            .filter_map(|context_ref| mention_from_protocol(&input.text, context_ref))
            .collect(),
        images: input
            .attachments
            .unwrap_or_default()
            .into_iter()
            .filter_map(image_from_protocol)
            .collect(),
    }
}

fn mention_from_protocol(
    text: &str,
    context_ref: protocol::ContextRef,
) -> Option<display_types::DisplayMention> {
    let label = context_ref.label();
    let target = context_ref.target().to_string();
    let (start_char, end_char) = find_reference_span(text, &label).unwrap_or((0, 0));
    let (kind, description) = match context_ref {
        protocol::ContextRef::File { .. } => (display_types::MentionKind::File, "file"),
        protocol::ContextRef::Directory { .. } => {
            (display_types::MentionKind::Directory, "directory")
        }
        protocol::ContextRef::Subagent { .. } => (display_types::MentionKind::Subagent, "subagent"),
        protocol::ContextRef::Url { .. } => return None,
    };

    Some(display_types::DisplayMention {
        start_char,
        end_char,
        kind,
        label,
        target,
        description: description.to_string(),
    })
}

fn find_reference_span(text: &str, label: &str) -> Option<(usize, usize)> {
    let needle = format!("@{label}");
    let byte_start = text.find(&needle)?;
    let start = text[..byte_start].chars().count();
    let end = start + needle.chars().count();
    Some((start, end))
}

fn image_from_protocol(
    attachment: protocol::AttachmentRef,
) -> Option<display_types::DisplayImageAttachment> {
    match attachment {
        protocol::AttachmentRef::LocalPath { path, name, .. } => {
            let file_name = name.unwrap_or_else(|| {
                Path::new(&path)
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .unwrap_or_default()
                    .to_string()
            });
            Some(display_types::DisplayImageAttachment {
                start_char: 0,
                end_char: 0,
                marker: String::new(),
                source_path: path,
                file_name,
            })
        }
        protocol::AttachmentRef::Uploaded { .. } => None,
    }
}

fn active_profile_from_protocol(profile: protocol::ActiveProfile) -> event_types::ActiveProfile {
    match profile {
        protocol::ActiveProfile::Main => event_types::ActiveProfile::Main,
        protocol::ActiveProfile::Auto => event_types::ActiveProfile::Auto,
        protocol::ActiveProfile::Plan => event_types::ActiveProfile::Plan,
    }
}

fn skill_source_kind_to_protocol(
    source_kind: crate::skills::SkillSourceKind,
) -> protocol::SkillSourceKind {
    match source_kind {
        crate::skills::SkillSourceKind::BuiltIn => protocol::SkillSourceKind::BuiltIn,
        crate::skills::SkillSourceKind::Project => protocol::SkillSourceKind::Project,
        crate::skills::SkillSourceKind::User => protocol::SkillSourceKind::User,
    }
}

fn skill_source_sort(source_kind: protocol::SkillSourceKind) -> u8 {
    match source_kind {
        protocol::SkillSourceKind::BuiltIn => 0,
        protocol::SkillSourceKind::Project => 1,
        protocol::SkillSourceKind::User => 2,
    }
}

fn thinking_effort_from_protocol(
    effort: protocol::ThinkingEffort,
) -> crate::types::config::ThinkingEffort {
    match effort {
        protocol::ThinkingEffort::None => crate::types::config::ThinkingEffort::None,
        protocol::ThinkingEffort::Low => crate::types::config::ThinkingEffort::Low,
        protocol::ThinkingEffort::Medium => crate::types::config::ThinkingEffort::Medium,
        protocol::ThinkingEffort::High => crate::types::config::ThinkingEffort::High,
    }
}

fn tool_pause_response_from_protocol(
    response: protocol::ToolPauseResponse,
) -> event_types::ToolPauseResponse {
    match response {
        protocol::ToolPauseResponse::Permission { approved, note } => {
            event_types::ToolPauseResponse::Permission { approved, note }
        }
        protocol::ToolPauseResponse::UserInput { value } => {
            event_types::ToolPauseResponse::UserInput { value }
        }
        protocol::ToolPauseResponse::Cancelled => event_types::ToolPauseResponse::Cancelled,
    }
}

fn plan_approval_action_from_protocol(
    action: protocol::PlanApprovalAction,
) -> event_types::PlanApprovalAction {
    match action {
        protocol::PlanApprovalAction::Approve { profile } => {
            event_types::PlanApprovalAction::Approve {
                profile: plan_execution_profile_from_protocol(profile),
            }
        }
        protocol::PlanApprovalAction::ApproveAndCompact { profile } => {
            event_types::PlanApprovalAction::ApproveAndCompact {
                profile: plan_execution_profile_from_protocol(profile),
            }
        }
        protocol::PlanApprovalAction::ContinueDiscussing => {
            event_types::PlanApprovalAction::ContinueDiscussing
        }
    }
}

fn plan_execution_profile_from_protocol(
    profile: protocol::PlanExecutionProfile,
) -> event_types::PlanExecutionProfile {
    match profile {
        protocol::PlanExecutionProfile::Main => event_types::PlanExecutionProfile::Main,
        protocol::PlanExecutionProfile::Auto => event_types::PlanExecutionProfile::Auto,
    }
}

fn agent_source_kind_from_protocol(
    source_kind: protocol::AgentSourceKind,
) -> subagent_types::AgentSourceKind {
    match source_kind {
        protocol::AgentSourceKind::BuiltIn => subagent_types::AgentSourceKind::BuiltIn,
        protocol::AgentSourceKind::Project => subagent_types::AgentSourceKind::Project,
        protocol::AgentSourceKind::User => subagent_types::AgentSourceKind::User,
    }
}

fn agent_source_kind_to_protocol(
    source_kind: subagent_types::AgentSourceKind,
) -> protocol::AgentSourceKind {
    match source_kind {
        subagent_types::AgentSourceKind::BuiltIn => protocol::AgentSourceKind::BuiltIn,
        subagent_types::AgentSourceKind::Project => protocol::AgentSourceKind::Project,
        subagent_types::AgentSourceKind::User => protocol::AgentSourceKind::User,
    }
}

fn agent_draft_from_protocol(draft: protocol::AgentDraft) -> subagent_types::AgentDraft {
    subagent_types::AgentDraft {
        name: draft.name,
        description: draft.description,
        instructions: draft.instructions,
        tools: draft.tools,
        disallow_tools: draft.disallow_tools,
        model: draft.model,
    }
}

fn provider_endpoint_to_protocol(
    endpoint: crate::types::config::ProviderType,
) -> protocol::ProviderEndpointKind {
    match endpoint {
        crate::types::config::ProviderType::OpenAI => protocol::ProviderEndpointKind::OpenAI,
        crate::types::config::ProviderType::Anthropic => protocol::ProviderEndpointKind::Anthropic,
    }
}

fn input_modality_to_protocol(
    modality: crate::types::config::InputModality,
) -> protocol::InputModality {
    match modality {
        crate::types::config::InputModality::Text => protocol::InputModality::Text,
        crate::types::config::InputModality::Image => protocol::InputModality::Image,
    }
}

fn models_response_from_settings(
    settings: &crate::types::config::Settings,
) -> protocol::ModelsResponse {
    let mut providers = settings
        .providers
        .iter()
        .map(|(id, provider)| protocol::ProviderInfo {
            id: id.clone(),
            name: provider.name.clone(),
            endpoint: provider_endpoint_to_protocol(provider.endpoint),
            base_url: provider.base_url.clone(),
            models: provider
                .models
                .iter()
                .map(|model| protocol::ModelInfo {
                    id: model.id.clone(),
                    name: model.name.clone(),
                    limit: model.limit,
                    thinking: model.thinking,
                    input_modalities: model.input_modalities.as_ref().map(|modalities| {
                        modalities
                            .iter()
                            .copied()
                            .map(input_modality_to_protocol)
                            .collect()
                    }),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    protocol::ModelsResponse {
        providers,
        current_provider: settings.active_provider.clone(),
        current_model: settings.model.clone(),
    }
}

fn agent_record_to_protocol(record: subagent_types::AgentRecord) -> protocol::AgentRecord {
    let id = record
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| record.name.clone());
    protocol::AgentRecord {
        id,
        name: record.name,
        description: record.description,
        instructions: record.instructions,
        tools: record.tools,
        disallow_tools: record.disallow_tools,
        model: record.model,
        source_kind: agent_source_kind_to_protocol(record.source_kind),
        editable: record.editable,
    }
}

#[cfg(test)]
mod tests {
    use crate::types::events as event_types;
    use omini_protocol as protocol;

    #[test]
    fn active_profile_changed_has_typed_overlay() {
        let event =
            event_types::RuntimeToUiEvent::ActiveProfileChanged(event_types::ActiveProfile::Plan);

        assert_eq!(
            super::key_runtime_event_from_internal(&event),
            Some(protocol::KeyRuntimeEvent::ActiveProfileChanged(
                protocol::ActiveProfileChangedEvent {
                    profile: protocol::ActiveProfile::Plan,
                },
            ))
        );
    }

    #[test]
    fn compact_summary_finished_has_typed_overlay() {
        let event = event_types::RuntimeToUiEvent::CompactSummaryFinished(
            event_types::CompactSummaryFinishedEvent {
                trigger: event_types::CompactTrigger::Manual,
                summary: "summary".to_string(),
                after_tokens: 42,
                session_id: Some("session_1".to_string()),
                agent_label: None,
            },
        );

        assert_eq!(
            super::key_runtime_event_from_internal(&event),
            Some(protocol::KeyRuntimeEvent::CompactSummaryFinished(
                protocol::CompactSummaryFinishedEvent {
                    trigger: protocol::CompactTrigger::Manual,
                    summary: "summary".to_string(),
                    after_tokens: 42,
                    session_id: Some("session_1".to_string()),
                    agent_label: None,
                },
            ))
        );
    }

    #[test]
    fn plan_approval_resolved_has_typed_overlay() {
        let event = event_types::RuntimeToUiEvent::PlanApprovalResolved {
            plan_id: "plan_1".to_string(),
            action: event_types::PlanApprovalAction::ContinueDiscussing,
        };

        assert_eq!(
            super::key_runtime_event_from_internal(&event),
            Some(protocol::KeyRuntimeEvent::PlanApprovalResolved(
                protocol::PlanApprovalResolvedEvent {
                    plan_id: "plan_1".to_string(),
                    action: protocol::PlanApprovalAction::ContinueDiscussing,
                },
            ))
        );
    }
}
