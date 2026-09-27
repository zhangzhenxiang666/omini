//! WebSocket 运行时事件协议。

use super::*;

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
    AgentRunChanged(AgentRunSnapshot),
    RunStarted,
    UserMessageInjected {
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    AgentTaskUserMessageQueued {
        task_id: String,
        thread_id: String,
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    AgentTaskMessageQueued {
        task_id: String,
        thread_id: String,
        item: HistoryItem,
    },
    RunFinished,
    Notification(NotificationEvent),
    ModelChanged(ModelChangedEvent),
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
    TaskChanged(TaskChangedEvent),
    TaskOutputDelta(TaskOutputDelta),
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
            Self::AgentRunChanged(_) => "agent_run_changed",
            Self::RunStarted => "run_started",
            Self::UserMessageInjected { .. } => "user_message_injected",
            Self::AgentTaskUserMessageQueued { .. } => "agent_task_user_message_queued",
            Self::AgentTaskMessageQueued { .. } => "agent_task_message_queued",
            Self::RunFinished => "run_finished",
            Self::Notification(_) => "notification",
            Self::ModelChanged(_) => "model_changed",
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
            Self::TaskChanged(_) => "task_changed",
            Self::TaskOutputDelta(_) => "task_output_delta",
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
