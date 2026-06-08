//! TUI 和本地 daemon 之间的 HTTP/WebSocket 协议类型。
//!
//! 这个 crate 只描述 wire shape；运行时状态、配置加载和 UI 展示逻辑分别留在
//! `omini-core`、`omini-server` 和 `omini-tui`。

use chrono::{DateTime, Utc};
pub use omini_domain::config::{
    InputModality, ModelInfo, ProviderEndpointKind, ProviderInfo, ThinkingEffort,
};
pub use omini_domain::display::HistoryItem;
pub use omini_domain::events::{
    ActiveProfile, CompactTrigger, PermissionPreview, PlanApprovalAction, PlanExecutionProfile,
    SessionRuntimeState, SessionSummary, SessionUsage, SessionUsageSnapshot, SubagentFinishedEvent,
    SubagentMessageEvent, SubagentSnapshot, SubagentStartedEvent, SubagentStatus,
    SubagentToolResultEvent, SubagentToolUseEvent, SubmittedPlan, ToolPauseKind, ToolPauseRequest,
    ToolPauseResponse,
};
pub use omini_domain::message::{ToolResultBlock, ToolUseBlock};
pub use omini_domain::subagents::{
    AgentDraft, AgentRecord as RuntimeAgentRecord, AgentSourceKind, AgentSummary,
    GeneratedAgentDraft,
};
use serde::{Deserialize, Serialize};

/// daemon 健康检查响应，用于客户端确认本地服务可用并识别服务名。
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

/// 客户端注册成功后返回的连接身份，后续 HTTP/WS 请求通过它关联同一个客户端。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterClientResponse {
    pub client_id: String,
}

/// 将一个项目工作目录挂到 daemon 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAttachRequest {
    pub cwd: String,
}

/// 项目 attach 的响应同时承担启动快照作用，TUI 用它初始化项目级 UI 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAttachResponse {
    /// daemon 内部用于路由项目级请求的稳定 ID。
    pub project_id: String,
    /// daemon 实际绑定的项目工作目录。
    pub cwd: String,
    /// 项目下可供 TUI 首屏展示或切换的会话列表。
    pub sessions: Vec<SessionSummary>,
    /// attach 时当前生效的 provider key。
    pub active_provider: String,
    /// attach 时当前生效的模型 ID。
    pub model: String,
    /// 当前模型的 thinking effort；不支持或未设置时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    /// 当前模型可用的上下文窗口；未知时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// 当前项目配置到 daemon 的 MCP server 数量。
    pub mcp_server_count: usize,
    /// 项目是否存在可注入的本地 instructions。
    pub has_project_instructions: bool,
    /// TUI 是否应默认展示 thinking 内容块。
    pub show_thinking_blocks: bool,
    /// attach 时可用于 @mention 或 agent 管理入口的 agent 摘要。
    pub agents: Vec<AgentSummary>,
    /// attach 时可用于 slash skill 列表的用户可调用 skill 摘要。
    pub skills: Vec<SkillSummary>,
}

/// 项目级运行配置更新后的快照；用于无活跃 session 的 TUI 状态同步。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRuntimeConfigResponse {
    pub active_provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    pub show_thinking_blocks: bool,
}

/// WebSocket runtime 事件直接承载 typed protocol event。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub event: TypedRuntimeEvent,
}

impl RuntimeEvent {
    pub fn new(event: TypedRuntimeEvent) -> Self {
        Self { event }
    }

    pub fn kind(&self) -> &'static str {
        self.event.kind()
    }
}

/// WebSocket runtime 事件的完整 typed 协议表示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TypedRuntimeEvent {
    RunStarted,
    UserMessageInjected {
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    RunFinished,
    Notification(NotificationEvent),
    ModelChanged(ModelChangedEvent),
    ThinkingDisplayChanged(ThinkingDisplayChangedEvent),
    UsageChanged(SessionUsageSnapshot),
    UsageTotalsChanged(UsageTotalsChangedEvent),
    ActiveProfileChanged(ActiveProfileChangedEvent),
    SessionTitleChanged(SessionTitleChangedEvent),
    ToolPauseRequested(ToolPauseRequestedEvent),
    PlanSubmitted(SubmittedPlan),
    PlanApprovalResolved(PlanApprovalResolvedEvent),
    AgentManagementUpdated {
        records: Vec<RuntimeAgentRecord>,
    },
    TurnStarted,
    TurnEnded,
    ThinkingDelta(RuntimeDeltaEvent),
    TextDelta(RuntimeDeltaEvent),
    ProposedPlanDelta(RuntimeDeltaEvent),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    CompactSummaryStarted(CompactSummaryStartedEvent),
    CompactSummaryDelta(CompactSummaryDeltaEvent),
    CompactSummaryFinished(CompactSummaryFinishedEvent),
    CompactSummaryFailed(CompactSummaryFailedEvent),
    SessionSnapshot(SessionSnapshotEvent),
    SubagentStarted(SubagentStartedEvent),
    SubagentMessageProduced(SubagentMessageEvent),
    SubagentToolUse(SubagentToolUseEvent),
    SubagentToolResult(SubagentToolResultEvent),
    SubagentFinished(SubagentFinishedEvent),
}

impl TypedRuntimeEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RunStarted => "run_started",
            Self::UserMessageInjected { .. } => "user_message_injected",
            Self::RunFinished => "run_finished",
            Self::Notification(_) => "notification",
            Self::ModelChanged(_) => "model_changed",
            Self::ThinkingDisplayChanged(_) => "thinking_display_changed",
            Self::UsageChanged(_) => "usage_changed",
            Self::UsageTotalsChanged(_) => "usage_totals_changed",
            Self::ActiveProfileChanged(_) => "active_profile_changed",
            Self::SessionTitleChanged(_) => "session_title_changed",
            Self::ToolPauseRequested(_) => "tool_pause_requested",
            Self::PlanSubmitted(_) => "plan_submitted",
            Self::PlanApprovalResolved(_) => "plan_approval_resolved",
            Self::AgentManagementUpdated { .. } => "agent_management_updated",
            Self::TurnStarted => "turn_started",
            Self::TurnEnded => "turn_ended",
            Self::ThinkingDelta(_) => "thinking_delta",
            Self::TextDelta(_) => "text_delta",
            Self::ProposedPlanDelta(_) => "proposed_plan_delta",
            Self::ToolUse(_) => "tool_use",
            Self::ToolResult(_) => "tool_result",
            Self::CompactSummaryStarted(_) => "compact_summary_started",
            Self::CompactSummaryDelta(_) => "compact_summary_delta",
            Self::CompactSummaryFinished(_) => "compact_summary_finished",
            Self::CompactSummaryFailed(_) => "compact_summary_failed",
            Self::SessionSnapshot(_) => "session_snapshot",
            Self::SubagentStarted(_) => "subagent_started",
            Self::SubagentMessageProduced(_) => "subagent_message_produced",
            Self::SubagentToolUse(_) => "subagent_tool_use",
            Self::SubagentToolResult(_) => "subagent_tool_result",
            Self::SubagentFinished(_) => "subagent_finished",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Warn,
    Error,
}

/// 面向客户端展示的通知事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationEvent {
    pub level: NotificationLevel,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

/// 当前会话模型配置已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChangedEvent {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

/// 当前会话 thinking 块显示偏好已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingDisplayChangedEvent {
    pub show: bool,
}

/// 当前会话累计 token usage 已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotalsChangedEvent {
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
}

/// 当前会话活跃 profile 已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProfileChangedEvent {
    pub profile: ActiveProfile,
}

/// 当前会话标题已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitleChangedEvent {
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPauseEventKind {
    Permission,
    UserInput,
}

/// runtime 暂停等待客户端处理工具请求时广播的完整事件。
pub type ToolPauseRequestedEvent = ToolPauseRequest;

/// plan mode 中模型提交给客户端审批的计划内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSubmittedEvent {
    pub plan_id: String,
    pub title: String,
    pub markdown: String,
}

/// 某个待确认计划已被处理，所有客户端都应关闭对应审批 UI。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanApprovalResolvedEvent {
    pub plan_id: String,
    pub action: PlanApprovalAction,
}

/// 流式输出增量事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDeltaEvent {
    pub delta: String,
}

/// 当前 session 开始 LLM 压缩摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryStartedEvent {
    pub trigger: CompactTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 当前 session 正在流式输出压缩摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryDeltaEvent {
    pub trigger: CompactTrigger,
    pub delta: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 当前 session 完成 LLM 压缩摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFinishedEvent {
    pub trigger: CompactTrigger,
    pub summary: String,
    pub after_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 当前 session LLM 压缩摘要失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 会话快照统计事件，用于重连或首屏同步时恢复概要状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshotEvent {
    /// 快照所属会话；旧事件可能不带该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub messages: Vec<HistoryItem>,
    pub subagents: Vec<SubagentSnapshot>,
    pub usage: SessionUsageSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeActivityKind {
    Query,
    Compact,
}

/// 当前会话正在执行的顶层活动及其计时信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeActivity {
    pub kind: SessionRuntimeActivityKind,
    pub started_at: DateTime<Utc>,
    /// 已运行时间，单位毫秒；query 活动会扣除等待客户端响应的暂停时长。
    pub elapsed_ms: u64,
}

/// 当前会话仍在等待处理的工具暂停项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimePendingPause {
    pub tool_use_id: String,
    pub tool_name: String,
    pub kind: ToolPauseEventKind,
    /// 暂停来自子 agent 时，这里标识源会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    /// 暂停来自子 agent 时，这里提供人类可读的 agent 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
}

/// 当前会话或子 agent 正在运行的工具调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeTool {
    pub tool_use_id: String,
    pub tool_name: String,
    pub started_at: DateTime<Utc>,
    /// 已运行时间，单位毫秒。
    pub elapsed_ms: u64,
    /// 工具来自子 agent 时，这里标识源会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    /// 工具来自子 agent 时，这里提供人类可读的 agent 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
}

/// 当前会话可见的 skill 运行态信息。
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

/// MCP server 暴露给模型的单个工具。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeMcpTool {
    pub name: String,
    /// 经过 daemon 去重后真正注册给模型使用的工具名。
    pub registered_name: String,
    pub description: String,
}

/// 当前会话可见的单个 MCP server 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeMcpServer {
    pub name: String,
    pub status: SessionRuntimeMcpStatus,
    /// 最近一次连接或初始化失败原因；非失败状态通常为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<SessionRuntimeMcpTool>,
}

/// 会话运行态的完整协议快照，供新连接或状态轮询同步 UI。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeStatus {
    pub session_id: String,
    /// 当前会话顶层运行状态。
    pub state: SessionRuntimeState,
    /// 当前会话使用的运行 profile。
    #[serde(default)]
    pub active_profile: ActiveProfile,
    /// core 是否已完成该会话的加载和 hydrate。
    pub loaded: bool,
    /// 当前拥有会话控制权的客户端 ID；无人控制时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    /// 当前订阅该会话事件流的客户端数量。
    pub connected_client_count: usize,
    /// 当前顶层活动；空表示没有正在运行或压缩的任务。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<SessionRuntimeActivity>,
    /// 所有尚未被客户端响应的暂停请求。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_pauses: Vec<SessionRuntimePendingPause>,
    /// 当前等待客户端确认的计划；用于新连接恢复计划审批抽屉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_plan_approval: Option<PlanSubmittedEvent>,
    /// 当前仍在执行的工具调用。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_tools: Vec<SessionRuntimeTool>,
    /// 当前会话可见的 skill 能力。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SessionRuntimeSkill>,
    /// 当前会话可见的 MCP server 能力和状态。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<SessionRuntimeMcpServer>,
    /// 当前会话可用的子 agent 能力列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagent_sessions: Vec<AgentSummary>,
}

/// 项目下多个活跃会话的运行态列表响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatusesResponse {
    pub statuses: Vec<SessionRuntimeStatus>,
}

/// 单个会话运行态查询响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRuntimeStatusResponse {
    pub status: SessionRuntimeStatus,
}

/// 用户输入在协议层携带语义化上下文引用，避免 TUI 把本地 mention 文本直接塞给 core。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInput {
    /// 用户输入的纯文本正文，不包含本地 UI mention 解析状态。
    pub text: String,
    /// TUI 已解析出的语义化上下文引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_refs: Option<Vec<ContextRef>>,
    /// 随本轮输入一起发送的附件引用。
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
    /// 指向项目中的文件路径。
    File {
        path: String,
        /// UI 展示用标签；为空时客户端可回退到路径。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// 指向项目中的目录路径。
    Directory {
        path: String,
        /// UI 展示用标签；为空时客户端可回退到路径。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// 指向一个可调用的子 agent。
    Subagent {
        name: String,
        /// UI 展示用标签；为空时客户端可回退到 agent 名称。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// 指向外部 URL。
    Url {
        url: String,
        /// UI 展示用标签；为空时客户端可回退到 URL。
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
    /// 客户端本地路径引用，server/core 可按需读取。
    LocalPath {
        path: String,
        /// 客户端已知的 MIME 类型；未知时为空。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// UI 展示或模型提示中使用的附件名称。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// 已上传到 daemon 的附件引用。
    Uploaded {
        attachment_id: String,
        mime_type: String,
        /// UI 展示或模型提示中使用的附件名称。
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

/// 当前会话可用模型列表及正在使用的模型选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

/// 设置当前会话 provider、模型和可选 thinking effort 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModelRequest {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
}

/// 设置当前会话 thinking effort 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetThinkingEffortRequest {
    pub effort: ThinkingEffort,
}

/// 设置当前会话活跃 provider profile 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetActiveProfileRequest {
    pub profile: ActiveProfile,
}

/// 设置当前会话 thinking 块显示偏好的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetThinkingDisplayRequest {
    /// 目标显示状态；为空时表示按当前偏好切换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<bool>,
}

/// 项目下可见会话列表响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsResponse {
    pub sessions: Vec<SessionSummary>,
}

/// 创建会话时可覆盖项目默认运行配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ActiveProfile>,
}

/// 创建会话后的响应；部分旧流程只清空当前会话，因此可能没有新 ID。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    /// 新建会话 ID；旧的清空式创建流程中为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// 重命名当前会话的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameSessionRequest {
    pub title: String,
}

/// 打开项目中某个已有会话的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSessionRequest {
    pub session_id: String,
}

/// 请求压缩当前会话上下文，可携带用户补充指令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactContextRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// agent 管理接口中的完整 agent 记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// 协议层稳定标识；可编辑 agent 通常是文件路径，内置 agent 可退回名称。
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
    /// 是否允许客户端基于该记录发起编辑或覆盖保存。
    pub editable: bool,
}

/// agent 管理页面的启动数据响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsResponse {
    pub records: Vec<AgentRecord>,
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

/// 保存 agent 草稿的请求，可用于创建或覆盖已有 agent。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveAgentRequest {
    pub source_kind: AgentSourceKind,
    /// 编辑已有 agent 时携带原记录 ID；创建新 agent 时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_agent_id: Option<String>,
    pub draft: AgentDraft,
}

/// 请求模型根据描述生成 agent 草稿。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateAgentRequest {
    pub description: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
}

/// 模型生成的 agent 草稿字段；工具策略、scope 和保存位置由客户端确认后另行保存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateAgentResponse {
    pub draft: GeneratedAgentDraft,
}

/// skill 列表中展示的轻量摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

/// 单个 skill 的完整内容，用于 TUI 展开 slash skill 调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    /// skill markdown 或说明正文。
    pub body: String,
    /// skill 所在目录，用于提示模型理解来源。
    pub directory: String,
    /// 是否允许用户通过 slash skill 直接调用。
    pub user_invocable: bool,
}

/// 可调用 skill 列表响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsResponse {
    pub skills: Vec<SkillSummary>,
}

/// 单个 skill 详情响应。
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

/// 向会话提交用户输入或运行中插入输入的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitRunRequest {
    pub input: UserInput,
    /// 发起方客户端用于关联本地 optimistic echo 与 runtime echo 的一次性 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_echo_id: Option<String>,
    /// 不传时按普通用户回合处理，以保持旧客户端兼容。
    #[serde(default, skip_serializing_if = "is_submit_run_mode")]
    pub mode: RunInputMode,
}

fn is_submit_run_mode(mode: &RunInputMode) -> bool {
    *mode == RunInputMode::Submit
}

/// 运行请求已被接受后的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSubmittedResponse {
    pub run_id: String,
}

/// 客户端响应工具暂停请求的请求体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveToolPauseRequest {
    pub response: ToolPauseResponse,
}

/// 客户端审批或拒绝模型提交计划的请求体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvePlanRequest {
    pub action: PlanApprovalAction,
}

/// 客户端声明或接管会话控制权后的租约状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerLease {
    pub client_id: String,
    /// 当前控制者客户端 ID；释放或无人控制时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
}

/// daemon 接收附件后返回的元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMetadata {
    pub attachment_id: String,
    pub mime_type: String,
    pub size: u64,
    pub name: String,
}

/// 附件上传接口响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentUploadResponse {
    pub attachment: AttachmentMetadata,
}

/// 无额外数据的成功响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckResponse {
    pub ok: bool,
}

impl AckResponse {
    pub fn ok() -> Self {
        Self { ok: true }
    }
}

/// HTTP API 的统一错误响应体。
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

/// WebSocket 外层 envelope 区分 runtime event 和 server 自己维护的连接/控制权状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEnvelope {
    /// typed runtime 事件。
    Event { event: RuntimeEvent },
    /// 连接建立时的实时运行状态快照；补足持久化 snapshot 不包含的运行中状态。
    RuntimeStatus { status: SessionRuntimeStatus },
    /// 会话控制权发生变化。
    ControllerChanged { controller_id: Option<String> },
    /// 当前连接的 controller/observer 角色发生变化。
    ClientRoleChanged {
        client_id: String,
        role: ClientSessionRole,
        /// 变化后的控制者客户端 ID；无人控制时为空。
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
            client_echo_id: None,
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
    fn submit_run_request_serializes_optional_client_echo_id() {
        let request = SubmitRunRequest {
            input: UserInput::plain("summarize this file"),
            client_echo_id: Some("echo-1".to_string()),
            mode: RunInputMode::Submit,
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "input": {
                    "text": "summarize this file"
                },
                "client_echo_id": "echo-1"
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
    fn sessions_response_carries_runtime_state() {
        let now = Utc::now();
        let response = SessionsResponse {
            sessions: vec![SessionSummary {
                id: "s1".to_string(),
                title: "hello".to_string(),
                model: "gpt-test".to_string(),
                provider: "openai".to_string(),
                created_at: now,
                updated_at: now,
                runtime_state: Some(SessionRuntimeState::Waiting),
            }],
        };

        assert_eq!(
            serde_json::to_value(response).unwrap()["sessions"][0]["runtime_state"],
            json!("waiting")
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
    fn generate_agent_request_requires_generation_model_fields() {
        let request = GenerateAgentRequest {
            description: "review diffs".to_string(),
            provider: "openai".to_string(),
            model: "reasoner".to_string(),
            thinking_effort: Some(ThinkingEffort::High),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "description": "review diffs",
                "provider": "openai",
                "model": "reasoner",
                "thinking_effort": "high"
            })
        );
    }

    #[test]
    fn generate_agent_response_contains_only_generated_fields() {
        let response = GenerateAgentResponse {
            draft: GeneratedAgentDraft {
                name: "diff-reviewer".to_string(),
                description: "Use when reviewing code diffs.".to_string(),
                instructions: "Review the diff and return findings.".to_string(),
            },
        };

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({
                "draft": {
                    "name": "diff-reviewer",
                    "description": "Use when reviewing code diffs.",
                    "instructions": "Review the diff and return findings."
                }
            })
        );
    }

    #[test]
    fn runtime_event_serializes_typed_event() {
        let event = RuntimeEvent::new(TypedRuntimeEvent::RunStarted);

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "event": {
                    "type": "run_started"
                }
            })
        );
    }

    #[test]
    fn active_profile_changed_event_uses_stable_semantic_fields() {
        let event = TypedRuntimeEvent::ActiveProfileChanged(ActiveProfileChangedEvent {
            profile: ActiveProfile::Plan,
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "active_profile_changed",
                "profile": "plan"
            })
        );
    }

    #[test]
    fn compact_summary_finished_event_uses_stable_semantic_fields() {
        let event = TypedRuntimeEvent::CompactSummaryFinished(CompactSummaryFinishedEvent {
            trigger: CompactTrigger::Manual,
            summary: "# Summary".to_string(),
            after_tokens: 42,
            session_id: Some("session_1".to_string()),
            agent_label: None,
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "compact_summary_finished",
                "trigger": "manual",
                "summary": "# Summary",
                "after_tokens": 42,
                "session_id": "session_1"
            })
        );
    }

    #[test]
    fn plan_approval_resolved_event_uses_stable_semantic_fields() {
        let event = TypedRuntimeEvent::PlanApprovalResolved(PlanApprovalResolvedEvent {
            plan_id: "plan_1".to_string(),
            action: PlanApprovalAction::ContinueDiscussing,
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "plan_approval_resolved",
                "plan_id": "plan_1",
                "action": {
                    "type": "continue_discussing"
                }
            })
        );
    }

    #[test]
    fn session_runtime_status_serializes_idle_snapshot() {
        let status = SessionRuntimeStatus {
            session_id: "s1".to_string(),
            state: SessionRuntimeState::Idle,
            active_profile: ActiveProfile::Main,
            loaded: false,
            controller_id: None,
            connected_client_count: 0,
            activity: None,
            pending_pauses: Vec::new(),
            pending_plan_approval: None,
            active_tools: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            subagent_sessions: Vec::new(),
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "session_id": "s1",
                "state": "idle",
                "active_profile": "main",
                "loaded": false,
                "connected_client_count": 0
            })
        );
    }

    #[test]
    fn session_runtime_status_serializes_pending_plan_approval() {
        let status = SessionRuntimeStatus {
            session_id: "s1".to_string(),
            state: SessionRuntimeState::Idle,
            active_profile: ActiveProfile::Main,
            loaded: true,
            controller_id: None,
            connected_client_count: 1,
            activity: None,
            pending_pauses: Vec::new(),
            pending_plan_approval: Some(PlanSubmittedEvent {
                plan_id: "plan_1".to_string(),
                title: "Plan".to_string(),
                markdown: "# Plan".to_string(),
            }),
            active_tools: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            subagent_sessions: Vec::new(),
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "session_id": "s1",
                "state": "idle",
                "active_profile": "main",
                "loaded": true,
                "connected_client_count": 1,
                "pending_plan_approval": {
                    "plan_id": "plan_1",
                    "title": "Plan",
                    "markdown": "# Plan"
                }
            })
        );
    }

    #[test]
    fn session_runtime_status_serializes_subagent_sessions() {
        let status = SessionRuntimeStatus {
            session_id: "s1".to_string(),
            state: SessionRuntimeState::Idle,
            active_profile: ActiveProfile::Main,
            loaded: true,
            controller_id: None,
            connected_client_count: 1,
            activity: None,
            pending_pauses: Vec::new(),
            pending_plan_approval: None,
            active_tools: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            subagent_sessions: vec![AgentSummary {
                name: "explorer".to_string(),
                description: "Read-only exploration agent.".to_string(),
            }],
        };

        let value = serde_json::to_value(status).unwrap();

        assert!(value.get("subagents").is_none());
        assert_eq!(
            value["subagent_sessions"],
            json!([{
                "name": "explorer",
                "description": "Read-only exploration agent."
            }])
        );
    }

    #[test]
    fn tool_pause_requested_event_carries_full_request() {
        let event = TypedRuntimeEvent::ToolPauseRequested(ToolPauseRequestedEvent {
            tool_use_id: "tool_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_session_id: Some("session_1".to_string()),
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Custom {
                tool_name: "bash".to_string(),
                payload: serde_json::Map::new(),
            }),
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "tool_pause_requested",
                "tool_use_id": "tool_1",
                "tool_name": "bash",
                "kind": {
                    "type": "permission",
                    "preview": {
                        "type": "custom",
                        "tool_name": "bash",
                        "payload": {}
                    }
                },
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

    #[test]
    fn runtime_status_envelope_serializes_connection_state_snapshot() {
        let envelope = ServerEnvelope::RuntimeStatus {
            status: SessionRuntimeStatus {
                session_id: "s1".to_string(),
                state: SessionRuntimeState::Compacting,
                active_profile: ActiveProfile::Auto,
                loaded: true,
                controller_id: Some("client_1".to_string()),
                connected_client_count: 2,
                activity: Some(SessionRuntimeActivity {
                    kind: SessionRuntimeActivityKind::Compact,
                    started_at: Utc::now(),
                    elapsed_ms: 250,
                }),
                pending_pauses: Vec::new(),
                pending_plan_approval: None,
                active_tools: Vec::new(),
                skills: Vec::new(),
                mcp_servers: Vec::new(),
                subagent_sessions: Vec::new(),
            },
        };
        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(value["type"], "runtime_status");
        assert_eq!(value["status"]["session_id"], "s1");
        assert_eq!(value["status"]["state"], "compacting");
        assert_eq!(value["status"]["activity"]["kind"], "compact");
    }
}
