//! TUI 和本地 daemon 之间的 HTTP/WebSocket 协议类型。
//!
//! 这个 crate 只描述 wire shape；运行时状态、配置加载和 UI 展示逻辑分别留在
//! `omini-core`、`omini-server` 和 `omini-tui`。

use chrono::{DateTime, Utc};
pub use omini_domain::config::{
    InputModality, ModelInfo, ProviderEndpointKind, ProviderInfo, ThinkingEffort,
};
pub use omini_domain::events::{
    ActiveProfile, PlanApprovalAction, PlanExecutionProfile, SessionSummary, SessionUsage,
    SubagentStatus, ToolPauseResponse,
};
pub use omini_domain::subagents::{AgentDraft, AgentSourceKind, AgentSummary};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHealthResponse {
    pub ok: bool,
    pub daemon: String,
}

/// 客户端注册请求目前只需要分配身份，kind 预留给后续区分 TUI/observer 等客户端。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterClientRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterClientResponse {
    pub client_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAttachRequest {
    pub cwd: String,
}

/// 项目 attach 的响应同时承担启动快照作用，TUI 用它初始化项目级 UI 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAttachResponse {
    pub project_id: String,
    pub cwd: String,
    pub sessions: Vec<SessionSummary>,
    pub active_provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    pub mcp_server_count: usize,
    pub has_project_instructions: bool,
    pub show_thinking_blocks: bool,
    pub agents: Vec<AgentSummary>,
    pub skills: Vec<SkillSummary>,
}

/// WebSocket runtime 事件保留 legacy payload，同时允许逐步增加稳定 typed overlay。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub kind: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<KeyRuntimeEvent>,
}

impl RuntimeEvent {
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
            event: None,
        }
    }

    pub fn with_key_event(mut self, event: KeyRuntimeEvent) -> Self {
        self.event = Some(event);
        self
    }
}

/// 跨客户端值得稳定消费的关键事件；其它 UI 细节可以暂时继续放在 legacy payload 中。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KeyRuntimeEvent {
    RunStarted,
    RunFinished,
    Notification(NotificationEvent),
    ToolPauseRequested(ToolPauseRequestedEvent),
    PlanSubmitted(PlanSubmittedEvent),
    SessionSnapshot(SessionSnapshotEvent),
    LegacyRuntime { kind: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationEvent {
    pub level: NotificationLevel,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPauseEventKind {
    Permission,
    UserInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolPauseRequestedEvent {
    pub tool_use_id: String,
    pub tool_name: String,
    pub kind: ToolPauseEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSubmittedEvent {
    pub plan_id: String,
    pub title: String,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshotEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub message_count: usize,
    pub subagent_count: usize,
    pub usage: SessionUsage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeState {
    #[default]
    Idle,
    Working,
    Thinking,
    Waiting,
    Compacting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeActivityKind {
    Query,
    Compact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeActivity {
    pub kind: SessionRuntimeActivityKind,
    pub started_at: DateTime<Utc>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimePendingPause {
    pub tool_use_id: String,
    pub tool_name: String,
    pub kind: ToolPauseEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeTool {
    pub tool_use_id: String,
    pub tool_name: String,
    pub started_at: DateTime<Utc>,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeSkill {
    pub name: String,
    pub description: String,
    pub source_kind: SkillSourceKind,
    pub directory: String,
    pub status: SessionRuntimeCapabilityStatus,
    pub inject: bool,
    pub user_invocable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceKind {
    BuiltIn,
    Project,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeCapabilityStatus {
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeMcpStatus {
    Disabled,
    Connecting,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeMcpTool {
    pub name: String,
    pub registered_name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeMcpServer {
    pub name: String,
    pub status: SessionRuntimeMcpStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<SessionRuntimeMcpTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeSubagent {
    pub session_id: String,
    pub agent_label: String,
    pub status: SubagentStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tool: Option<SessionRuntimeTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeStatus {
    pub session_id: String,
    pub state: SessionRuntimeState,
    pub loaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    pub connected_client_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionRuntimeActivity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_pauses: Vec<SessionRuntimePendingPause>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_tools: Vec<SessionRuntimeTool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SessionRuntimeSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<SessionRuntimeMcpServer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<SessionRuntimeSubagent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatusesResponse {
    pub statuses: Vec<SessionRuntimeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeStatusResponse {
    pub status: SessionRuntimeStatus,
}

/// 用户输入在协议层携带语义化上下文引用，避免 TUI 把本地 mention 文本直接塞给 core。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_refs: Option<Vec<ContextRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<AttachmentRef>>,
}

impl UserInput {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            context_refs: None,
            attachments: None,
        }
    }
}

/// TUI 中的 @mention 会在发送前转成 ContextRef，server/core 再决定如何读取或使用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextRef {
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Directory {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Subagent {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl ContextRef {
    pub fn label(&self) -> String {
        match self {
            Self::File { path, label } | Self::Directory { path, label } => {
                label.clone().unwrap_or_else(|| path.clone())
            }
            Self::Subagent { name, label } => label.clone().unwrap_or_else(|| name.clone()),
            Self::Url { url, label } => label.clone().unwrap_or_else(|| url.clone()),
        }
    }

    pub fn target(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Directory { path, .. } => path,
            Self::Subagent { name, .. } => name,
            Self::Url { url, .. } => url,
        }
    }
}

/// 附件既可以是本地路径，也可以是已经上传到 daemon 的引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentRef {
    LocalPath {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Uploaded {
        attachment_id: String,
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl AttachmentRef {
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::LocalPath { name, .. } | Self::Uploaded { name, .. } => name.as_deref(),
        }
    }

    pub fn mime_type(&self) -> Option<&str> {
        match self {
            Self::LocalPath { mime_type, .. } => mime_type.as_deref(),
            Self::Uploaded { mime_type, .. } => Some(mime_type.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModelRequest {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetThinkingEffortRequest {
    pub effort: ThinkingEffort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetActiveProfileRequest {
    pub profile: ActiveProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetThinkingDisplayRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsResponse {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSessionRequest {
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactContextRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallow_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub source_kind: AgentSourceKind,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsResponse {
    pub records: Vec<AgentRecord>,
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveAgentRequest {
    pub source_kind: AgentSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_agent_id: Option<String>,
    pub draft: AgentDraft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateAgentRequest {
    pub source_kind: AgentSourceKind,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallow_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    pub body: String,
    pub directory: String,
    pub user_invocable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsResponse {
    pub skills: Vec<SkillSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillResponse {
    pub skill: SkillDetail,
}

/// Submit 是普通用户回合；Intervene 用于运行中插入输入，默认保持旧客户端请求体兼容。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunInputMode {
    #[default]
    Submit,
    Intervene,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitRunRequest {
    pub input: UserInput,
    #[serde(default, skip_serializing_if = "is_submit_run_mode")]
    pub mode: RunInputMode,
}

fn is_submit_run_mode(mode: &RunInputMode) -> bool {
    *mode == RunInputMode::Submit
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSubmittedResponse {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveToolPauseRequest {
    pub response: ToolPauseResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvePlanRequest {
    pub action: PlanApprovalAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerLease {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMetadata {
    pub attachment_id: String,
    pub mime_type: String,
    pub size: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentUploadResponse {
    pub attachment: AttachmentMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckResponse {
    pub ok: bool,
}

impl AckResponse {
    pub fn ok() -> Self {
        Self { ok: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientSessionRole {
    Controller,
    Observer,
}

/// WebSocket 外层 envelope 区分 runtime payload 和 server 自己维护的连接/控制权状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEnvelope {
    Event {
        event: RuntimeEvent,
    },
    ControllerChanged {
        controller_id: Option<String>,
    },
    ClientRoleChanged {
        client_id: String,
        role: ClientSessionRole,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller_id: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn submit_run_request_serializes_as_endpoint_body() {
        let request = SubmitRunRequest {
            input: UserInput::plain("summarize this file"),
            mode: RunInputMode::Submit,
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "input": {
                    "text": "summarize this file"
                }
            })
        );
    }

    #[test]
    fn daemon_health_serializes_as_identity_probe() {
        let response = DaemonHealthResponse {
            ok: true,
            daemon: "omini".to_string(),
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "ok": true,
                "daemon": "omini"
            })
        );
    }

    #[test]
    fn project_attach_request_carries_real_cwd() {
        let request = ProjectAttachRequest {
            cwd: "/tmp/my project".to_string(),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "cwd": "/tmp/my project"
            })
        );
    }

    #[test]
    fn uploaded_attachment_uses_attachment_id() {
        let attachment = AttachmentRef::Uploaded {
            attachment_id: "att_1".to_string(),
            mime_type: "image/png".to_string(),
            name: Some("diagram.png".to_string()),
        };

        assert_eq!(
            serde_json::to_value(attachment).unwrap(),
            json!({
                "kind": "uploaded",
                "attachment_id": "att_1",
                "mime_type": "image/png",
                "name": "diagram.png"
            })
        );
    }

    #[test]
    fn runtime_event_can_carry_typed_key_event_overlay() {
        let event = RuntimeEvent::new(
            "run_started",
            json!({
                "type": "run_started"
            }),
        )
        .with_key_event(KeyRuntimeEvent::RunStarted);

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "kind": "run_started",
                "payload": {
                    "type": "run_started"
                },
                "event": {
                    "type": "run_started"
                }
            })
        );
    }

    #[test]
    fn session_runtime_status_serializes_idle_snapshot() {
        let status = SessionRuntimeStatus {
            session_id: "s1".to_string(),
            state: SessionRuntimeState::Idle,
            loaded: false,
            controller_id: None,
            connected_client_count: 0,
            activity: None,
            pending_pauses: Vec::new(),
            active_tools: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            subagents: Vec::new(),
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "session_id": "s1",
                "state": "idle",
                "loaded": false,
                "connected_client_count": 0
            })
        );
    }

    #[test]
    fn tool_pause_requested_event_uses_stable_semantic_fields() {
        let event = KeyRuntimeEvent::ToolPauseRequested(ToolPauseRequestedEvent {
            tool_use_id: "tool_1".to_string(),
            tool_name: "bash".to_string(),
            kind: ToolPauseEventKind::Permission,
            source_session_id: Some("session_1".to_string()),
            source_agent_label: None,
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "tool_pause_requested",
                "tool_use_id": "tool_1",
                "tool_name": "bash",
                "kind": "permission",
                "source_session_id": "session_1"
            })
        );
    }

    #[test]
    fn client_role_changed_envelope_serializes_role_for_one_client() {
        let envelope = ServerEnvelope::ClientRoleChanged {
            client_id: "client_1".to_string(),
            role: ClientSessionRole::Controller,
            controller_id: Some("client_1".to_string()),
        };

        assert_eq!(
            serde_json::to_value(envelope).unwrap(),
            json!({
                "type": "client_role_changed",
                "client_id": "client_1",
                "role": "controller",
                "controller_id": "client_1"
            })
        );
    }
}
