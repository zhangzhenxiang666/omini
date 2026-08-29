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
    pub bundled_rg: BundledToolStatus,
}

/// server 管理的内置依赖状态；客户端只展示状态，不负责下载或修复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundledToolState {
    Ready,
    Restoring,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundledToolStatus {
    pub state: BundledToolState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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

/// 一个项目按「全局配置 + 项目覆盖」合并后的可运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectConfigurationState {
    Ready,
    SetupRequired,
    Invalid,
}

/// 供所有客户端决定显示普通工作区、首次引导还是只读诊断页的项目配置快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfigurationResponse {
    pub state: ProjectConfigurationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// provider 缺少模型时，客户端可以预填该 ID；不包含任何 secret。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// 服务端首次配置入口。api_key 仅用于本次写入 auth.json，绝不出现在响应中。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapProjectConfigurationRequest {
    pub provider_id: String,
    pub protocol: ProviderEndpointKind,
    pub base_url: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_variable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for BootstrapProjectConfigurationRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapProjectConfigurationRequest")
            .field("provider_id", &self.provider_id)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("model_id", &self.model_id)
            .field("environment_variable", &self.environment_variable)
            .field("api_key", &self.api_key.as_ref().map(|_| "REDACTED"))
            .finish()
    }
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
    pub thread_id: String,
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

/// 创建线程后的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateThreadResponse {
    pub thread_id: String,
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

/// Submit 是普通用户回合；Intervene 用于运行中插入输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunInputMode {
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
    pub mode: RunInputMode,
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
