//! client 和本地 daemon 之间的 HTTP/WebSocket 协议类型。
//!
//! 这个 crate 只描述 wire shape；运行时状态、配置加载和 UI 展示逻辑分别留在
//! `omini-core`、`omini-server` 和 `omini-tui`。

use chrono::{DateTime, Utc};
pub use omini_domain::config::{
    InputModality, ModelInfo, ProviderEndpointKind, ProviderInfo, ThinkingEffort,
};
pub use omini_domain::display::HistoryItem;
pub use omini_domain::events::{
    ActiveProfile, AgentTaskEvent, AgentTaskEventEnvelope, AgentTaskExecutionMode, AgentTaskInfo,
    AgentTaskResult, AgentTaskSnapshot, AgentTaskStatus, CompactTrigger, MAX_AGENT_DEPTH,
    PermissionPreview, PlanApprovalAction, PlanExecutionProfile, SubmittedPlan, ThreadRuntimeState,
    ThreadSummary, ThreadUsage, ThreadUsageSnapshot, ToolPauseKind, ToolPauseRequest,
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

/// 项目路径的即时可用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectPathStatus {
    Ready,
    Missing,
}

/// 一个已持久化注册的项目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub storage_key: String,
    pub path_status: ProjectPathStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectSummary>,
}

/// 注册当前真实工作目录；同一 canonical path 的请求是幂等的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProjectRequest {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 修改项目展示名称，或显式 relink 到新的真实目录。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateProjectRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// 打开项目后供客户端初始化的完整快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenProjectResponse {
    pub project: ProjectSummary,
    /// 项目下可供 TUI 首屏展示或切换的线程列表。
    pub threads: Vec<ThreadSummary>,
    /// open 时当前生效的 provider key。
    pub active_provider: String,
    /// open 时当前生效的模型 ID。
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
    /// open 时可用于 @mention 或 agent 管理入口的 agent 摘要。
    pub agents: Vec<AgentSummary>,
    /// open 时可用于 slash skill 列表的用户可调用 skill 摘要。
    pub skills: Vec<SkillSummary>,
    /// 项目工作目录的 git 分支；不在 git 仓库中时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
}

/// 项目级运行配置更新后的快照；用于无活跃 thread 的 TUI 状态同步。
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
    UsageChanged(ThreadUsageSnapshot),
    UsageTotalsChanged(UsageTotalsChangedEvent),
    ActiveProfileChanged(ActiveProfileChangedEvent),
    ThreadTitleChanged(ThreadTitleChangedEvent),
    ToolPauseRequested(ToolPauseRequest),
    PlanSubmitted(SubmittedPlan),
    PlanApprovalResolved(PlanApprovalResolvedEvent),
    AgentManagementUpdated {
        records: Vec<RuntimeAgentRecord>,
    },
    TurnStarted,
    TurnEnded,
    GitBranchChanged(GitBranchChangedEvent),
    ThinkingDelta(RuntimeDeltaEvent),
    TextDelta(RuntimeDeltaEvent),
    ProposedPlanDelta(RuntimeDeltaEvent),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    CompactSummaryStarted(CompactSummaryStartedEvent),
    CompactSummaryDelta(CompactSummaryDeltaEvent),
    CompactSummaryFinished(CompactSummaryFinishedEvent),
    CompactSummaryFailed(CompactSummaryFailedEvent),
    ThreadSnapshot(ThreadSnapshotEvent),
    /// 「在新线程中执行计划」审批通过后,server 在 fork 出新 RuntimeThread 后
    /// 通过普通 runtime event 通道广播给所有客户端。TUI 收到后应重连到 `to` 的 ws;
    /// 旧 thread 的 runtime 活动自然结束,由 server 的 reclaim 机制回收。
    ThreadSwitched(ThreadSwitchedEvent),
    AgentTaskEvent(AgentTaskEventEnvelope),
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
            Self::ThreadTitleChanged(_) => "thread_title_changed",
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
            Self::GitBranchChanged(_) => "git_branch_changed",
            Self::CompactSummaryStarted(_) => "compact_summary_started",
            Self::CompactSummaryDelta(_) => "compact_summary_delta",
            Self::CompactSummaryFinished(_) => "compact_summary_finished",
            Self::CompactSummaryFailed(_) => "compact_summary_failed",
            Self::ThreadSnapshot(_) => "thread_snapshot",
            Self::ThreadSwitched(_) => "thread_switched",
            Self::AgentTaskEvent(_) => "agent_task_event",
        }
    }
}

/// 「在新线程中执行计划」触发 thread 切换时广播的事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSwitchedEvent {
    /// 切换前的 thread ID。
    pub from: String,
    /// 切换后的 thread ID。
    pub to: String,
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

/// 当前线程模型配置已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChangedEvent {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

/// 当前线程 thinking 块显示偏好已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingDisplayChangedEvent {
    pub show: bool,
}

/// 当前线程累计 token usage 已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotalsChangedEvent {
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
}

/// 当前线程活跃 profile 已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProfileChangedEvent {
    pub profile: ActiveProfile,
}

/// 当前线程标题已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadTitleChangedEvent {
    pub title: Option<String>,
}

/// 当前 git 分支已变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchChangedEvent {
    pub branch: Option<String>,
}

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

/// 当前 thread 开始 LLM 压缩摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryStartedEvent {
    pub trigger: CompactTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 当前 thread 正在流式输出压缩摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryDeltaEvent {
    pub trigger: CompactTrigger,
    pub delta: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 当前 thread 完成 LLM 压缩摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFinishedEvent {
    pub trigger: CompactTrigger,
    pub summary: String,
    pub after_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 当前 thread LLM 压缩摘要失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
}

/// 线程快照统计事件，用于重连或首屏同步时恢复概要状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSnapshotEvent {
    /// 快照所属线程；旧事件可能不带该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub messages: Vec<HistoryItem>,
    pub agent_tasks: Vec<AgentTaskSnapshot>,
    pub usage: ThreadUsageSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRuntimeActivityKind {
    Query,
    Compact,
}

/// 当前线程正在执行的顶层活动及其计时信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRuntimeActivity {
    pub kind: ThreadRuntimeActivityKind,
    pub started_at: DateTime<Utc>,
    /// 已运行时间，单位毫秒；query 活动会扣除等待客户端响应的暂停时长。
    pub elapsed_ms: u64,
}

/// 当前线程或子 agent 正在运行的工具调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRuntimeTool {
    pub tool_use_id: String,
    pub tool_name: String,
    pub started_at: DateTime<Utc>,
    /// 已运行时间，单位毫秒。
    pub elapsed_ms: u64,
    /// 工具来自子 agent 时，这里标识源线程。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<String>,
    /// 工具来自子 agent 时，这里提供人类可读的 agent 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
}

/// 当前线程可见的 skill 运行态信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRuntimeSkill {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub source_kind: SkillSourceKind,
    pub directory: String,
    pub status: ThreadRuntimeCapabilityStatus,
    pub disable_model_invocation: bool,
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
pub enum ThreadRuntimeCapabilityStatus {
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRuntimeMcpStatus {
    Disabled,
    Connecting,
    Ready,
    Failed,
}

/// MCP server 暴露给模型的单个工具。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRuntimeMcpTool {
    pub name: String,
    /// 经过 daemon 去重后真正注册给模型使用的工具名。
    pub registered_name: String,
    pub description: String,
}

/// 当前线程可见的单个 MCP server 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRuntimeMcpServer {
    pub name: String,
    pub status: ThreadRuntimeMcpStatus,
    /// 最近一次连接或初始化失败原因；非失败状态通常为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ThreadRuntimeMcpTool>,
}

/// 线程运行态的完整协议快照，供新连接或状态轮询同步 UI。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadRuntimeStatus {
    pub thread_id: String,
    /// 当前线程顶层运行状态。
    pub state: ThreadRuntimeState,
    /// 当前线程使用的运行 profile。
    #[serde(default)]
    pub active_profile: ActiveProfile,
    /// core 是否已完成该线程的加载和 hydrate。
    pub loaded: bool,
    /// 当前拥有线程控制权的客户端 ID；无人控制时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    /// 当前订阅该线程事件流的客户端数量。
    pub connected_client_count: usize,
    /// 当前顶层活动；空表示没有正在运行或压缩的任务。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<ThreadRuntimeActivity>,
    /// 所有尚未被客户端响应的暂停请求。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_pauses: Vec<ToolPauseRequest>,
    /// 当前等待客户端确认的计划；用于新连接恢复计划审批抽屉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_plan_approval: Option<PlanSubmittedEvent>,
    /// 当前仍在执行的工具调用。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_tools: Vec<ThreadRuntimeTool>,
    /// 当前线程可见的 skill 能力。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<ThreadRuntimeSkill>,
    /// 当前线程可见的 MCP server 能力和状态。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<ThreadRuntimeMcpServer>,
    /// 当前线程可用的子 agent 能力列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagent_threads: Vec<AgentSummary>,
    /// 当前工作目录的 git 分支；不在 git 仓库中时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
}

/// 项目下多个活跃线程的运行态列表响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadStatusesResponse {
    pub statuses: Vec<ThreadRuntimeStatus>,
}

/// 单个线程运行态查询响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadRuntimeStatusResponse {
    pub status: ThreadRuntimeStatus,
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

/// 当前线程可用模型列表及正在使用的模型选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

/// 设置当前线程 provider、模型和可选 thinking effort 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModelRequest {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
}

/// 设置当前线程 thinking effort 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetThinkingEffortRequest {
    pub effort: ThinkingEffort,
}

/// 设置当前线程活跃 provider profile 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetActiveProfileRequest {
    pub profile: ActiveProfile,
}

/// 设置当前线程 thinking 块显示偏好的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetThinkingDisplayRequest {
    /// 目标显示状态；为空时表示按当前偏好切换。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<bool>,
}

/// 项目下可见线程列表响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadsResponse {
    pub threads: Vec<ThreadSummary>,
}

/// TODO: 注意这里如果provider为Some但是model为None那么可能会出问题
/// 创建线程时可覆盖项目默认运行配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateThreadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ActiveProfile>,
}

/// 创建线程后的响应；部分旧流程只清空当前线程，因此可能没有新 ID。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateThreadResponse {
    /// 新建线程 ID；旧的清空式创建流程中为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

/// 重命名当前线程的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameThreadRequest {
    pub title: String,
}

/// 打开项目中某个已有线程的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenThreadRequest {
    pub thread_id: String,
}

/// 请求压缩当前线程上下文，可携带用户补充指令。
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
}

/// 单个 skill 的完整内容，用于 TUI 展开 slash skill 调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    /// 短描述，用于在命令面板中显示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
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

/// 向线程提交用户输入或运行中插入输入的请求。
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

/// 客户端声明或接管线程控制权后的租约状态。
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
pub enum ClientThreadRole {
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
    RuntimeStatus { status: ThreadRuntimeStatus },
    /// 线程控制权发生变化。
    ControllerChanged { controller_id: Option<String> },
    /// 当前连接的 controller/observer 角色发生变化。
    ClientRoleChanged {
        client_id: String,
        role: ClientThreadRole,
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
    fn threads_response_carries_runtime_state() {
        let now = Utc::now();
        let response = ThreadsResponse {
            threads: vec![ThreadSummary {
                id: "s1".to_string(),
                title: "hello".to_string(),
                model: "gpt-test".to_string(),
                provider: "openai".to_string(),
                created_at: now,
                updated_at: now,
                runtime_state: Some(ThreadRuntimeState::Waiting),
            }],
        };

        assert_eq!(
            serde_json::to_value(response).unwrap()["threads"][0]["runtime_state"],
            json!("waiting")
        );
    }

    #[test]
    fn create_project_request_carries_path_and_optional_name() {
        let request = CreateProjectRequest {
            path: "/tmp/my project".to_string(),
            name: Some("My project".to_string()),
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "path": "/tmp/my project",
                "name": "My project"
            })
        );
    }

    #[test]
    fn project_path_status_uses_wire_names() {
        assert_eq!(
            serde_json::to_value(ProjectPathStatus::Ready).unwrap(),
            json!("ready")
        );
        assert_eq!(
            serde_json::to_value(ProjectPathStatus::Missing).unwrap(),
            json!("missing")
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
                short_description: None,
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
    fn agent_task_event_envelope_round_trips_with_stable_wire_shape() {
        let event = RuntimeEvent::new(TypedRuntimeEvent::AgentTaskEvent(AgentTaskEventEnvelope {
            task_id: "task_1".to_string(),
            thread_id: "thread_1".to_string(),
            parent_task_id: Some("task_parent".to_string()),
            owner_thread_id: "owner_1".to_string(),
            truncated: false,
            payload: AgentTaskEvent::TextDelta {
                delta: "hello".to_string(),
            },
        }));

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event"]["type"], json!("agent_task_event"));
        assert_eq!(value["event"]["payload"]["type"], json!("text_delta"));
        assert_eq!(value["event"]["payload"]["delta"], json!("hello"));
        assert_eq!(
            serde_json::from_value::<RuntimeEvent>(value).unwrap(),
            event
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
            thread_id: Some("thread_1".to_string()),
            agent_label: None,
        });

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({
                "type": "compact_summary_finished",
                "trigger": "manual",
                "summary": "# Summary",
                "after_tokens": 42,
                "thread_id": "thread_1"
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
    fn thread_runtime_status_serializes_idle_snapshot() {
        let status = ThreadRuntimeStatus {
            thread_id: "s1".to_string(),
            state: ThreadRuntimeState::Idle,
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
            subagent_threads: Vec::new(),
            git_branch: None,
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "thread_id": "s1",
                "state": "idle",
                "active_profile": "main",
                "loaded": false,
                "connected_client_count": 0
            })
        );
    }

    #[test]
    fn thread_runtime_status_serializes_pending_plan_approval() {
        let status = ThreadRuntimeStatus {
            thread_id: "s1".to_string(),
            state: ThreadRuntimeState::Idle,
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
            subagent_threads: Vec::new(),
            git_branch: None,
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "thread_id": "s1",
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
    fn thread_runtime_status_serializes_subagent_threads() {
        let status = ThreadRuntimeStatus {
            thread_id: "s1".to_string(),
            state: ThreadRuntimeState::Idle,
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
            subagent_threads: vec![AgentSummary {
                name: "explorer".to_string(),
                description: "Read-only exploration agent.".to_string(),
                short_description: None,
                location: "<built-in>".to_string(),
            }],
            git_branch: None,
        };

        let value = serde_json::to_value(status).unwrap();

        assert!(value.get("subagents").is_none());
        assert_eq!(
            value["subagent_threads"],
            json!([{
                "name": "explorer",
                "description": "Read-only exploration agent.",
                "location": "<built-in>",
            }])
        );
    }

    #[test]
    fn tool_pause_requested_event_carries_full_request() {
        let event = TypedRuntimeEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "tool_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_thread_id: Some("thread_1".to_string()),
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
                "source_thread_id": "thread_1"
            })
        );
    }

    #[test]
    fn client_role_changed_envelope_serializes_role_for_one_client() {
        let envelope = ServerEnvelope::ClientRoleChanged {
            client_id: "client_1".to_string(),
            role: ClientThreadRole::Controller,
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
    fn thread_switched_typed_event_round_trips() {
        let event = TypedRuntimeEvent::ThreadSwitched(ThreadSwitchedEvent {
            from: "thread_old".to_string(),
            to: "thread_new".to_string(),
        });

        // 事件嵌入 RuntimeEvent 顶层,随 `ServerEnvelope::Event` 走普通 runtime 通道。
        let runtime_event = RuntimeEvent::new(event.clone());
        let value = serde_json::to_value(runtime_event).unwrap();
        assert_eq!(
            value,
            json!({
                "event": {
                    "type": "thread_switched",
                    "from": "thread_old",
                    "to": "thread_new"
                }
            })
        );

        // 反向 round-trip:TUI 端 `runtime_event_from_protocol` 依赖 Decode 阶段正确解析 from/to。
        let decoded: RuntimeEvent = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.event, event);
    }

    #[test]
    fn runtime_status_envelope_serializes_connection_state_snapshot() {
        let envelope = ServerEnvelope::RuntimeStatus {
            status: ThreadRuntimeStatus {
                thread_id: "s1".to_string(),
                state: ThreadRuntimeState::Compacting,
                active_profile: ActiveProfile::Auto,
                loaded: true,
                controller_id: Some("client_1".to_string()),
                connected_client_count: 2,
                activity: Some(ThreadRuntimeActivity {
                    kind: ThreadRuntimeActivityKind::Compact,
                    started_at: Utc::now(),
                    elapsed_ms: 250,
                }),
                pending_pauses: Vec::new(),
                pending_plan_approval: None,
                active_tools: Vec::new(),
                skills: Vec::new(),
                mcp_servers: Vec::new(),
                subagent_threads: Vec::new(),
                git_branch: None,
            },
        };
        let value = serde_json::to_value(envelope).unwrap();

        assert_eq!(value["type"], "runtime_status");
        assert_eq!(value["status"]["thread_id"], "s1");
        assert_eq!(value["status"]["state"], "compacting");
        assert_eq!(value["status"]["activity"]["kind"], "compact");
    }
}
