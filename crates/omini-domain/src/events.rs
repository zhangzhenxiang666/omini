use crate::config::ThinkingEffort;
use crate::display::{DisplayPlan, HistoryItem};
use crate::message::{Message, ToolResultBlock, ToolUseBlock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveProfile {
    #[default]
    Main,
    Auto,
    Plan,
}

impl ActiveProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Auto => "auto",
            Self::Plan => "plan",
        }
    }
}

impl std::fmt::Display for ActiveProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub kind: NotificationKind,
    pub message: String,
    pub details: Vec<String>,
}

impl Notification {
    pub fn new(kind: NotificationKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Vec::new(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(NotificationKind::Info, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(NotificationKind::Warn, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(NotificationKind::Error, message)
    }

    pub fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

pub const MAX_AGENT_DEPTH: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskExecutionMode {
    Background,
    Synchronous,
}

impl AgentTaskExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Synchronous => "synchronous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskStatus {
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl AgentTaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskInfo {
    pub task_id: String,
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    pub owner_thread_id: String,
    pub parent_thread_id: String,
    pub spawn_tool_use_id: String,
    pub agent: String,
    pub title: String,
    pub depth: u8,
    pub execution_mode: AgentTaskExecutionMode,
    pub status: AgentTaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<AgentTaskResult>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notification_delivered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskSnapshot {
    #[serde(flatten)]
    pub task: AgentTaskInfo,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskEventEnvelope {
    pub task_id: String,
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    pub owner_thread_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    pub payload: AgentTaskEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentTaskEvent {
    Started {
        parent_thread_id: String,
        spawn_tool_use_id: String,
        agent: String,
        title: String,
        depth: u8,
        execution_mode: AgentTaskExecutionMode,
    },
    TurnStarted,
    ThinkingDelta {
        delta: String,
    },
    TextDelta {
        delta: String,
    },
    ToolUse {
        tool_use: ToolUseBlock,
    },
    ToolResult {
        tool_result: ToolResultBlock,
    },
    MessageCommitted {
        message: Message,
        #[serde(default)]
        persist_llm_history: bool,
    },
    TurnEnded,
    Finished {
        status: AgentTaskStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<AgentTaskResult>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    Auto,
    Manual,
}

impl CompactTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

impl std::fmt::Display for CompactTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactEvent {
    pub trigger: CompactTrigger,
    pub thread_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryDeltaEvent {
    pub trigger: CompactTrigger,
    pub delta: String,
    pub thread_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactShrinkFinishedEvent {
    pub trigger: CompactTrigger,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub before_messages: usize,
    pub after_messages: usize,
    pub thread_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactShrinkFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    pub thread_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFinishedEvent {
    pub trigger: CompactTrigger,
    pub summary: String,
    pub after_tokens: usize,
    pub thread_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    pub thread_id: Option<String>,
    pub agent_label: Option<String>,
}

pub type SubmittedPlan = DisplayPlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutionProfile {
    Main,
    Auto,
}

impl PlanExecutionProfile {
    pub fn active_profile(self) -> ActiveProfile {
        match self {
            Self::Main => ActiveProfile::Main,
            Self::Auto => ActiveProfile::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanApprovalAction {
    Approve {
        profile: PlanExecutionProfile,
    },
    /// 「在新线程中执行计划」:client 选此 action 时,server 路由层会直接读
    /// plan 文件并 fork 新 `RuntimeThread`,不进入 core 的审批状态机;
    /// core 收到此 action 后只发出 resolved 事件关闭抽屉,状态保持。
    ApproveInNewThread {
        profile: PlanExecutionProfile,
    },
    ContinueDiscussing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_state: Option<ThreadRuntimeState>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRuntimeState {
    #[default]
    Idle,
    Working,
    Thinking,
    Waiting,
    Compacting,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadUsageSnapshot {
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadUsage {
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

impl From<ThreadUsageSnapshot> for ThreadUsage {
    fn from(usage: ThreadUsageSnapshot) -> Self {
        Self {
            current_context_tokens: usage.current_context_tokens,
            total_tokens: usage.total_tokens,
            total_cached_tokens: usage.total_cached_tokens,
            context_window: usage.context_window,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedThread {
    pub thread_id: String,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default)]
    pub active_profile: ActiveProfile,
    pub title: Option<String>,
    pub messages: Vec<HistoryItem>,
    pub agent_tasks: Vec<AgentTaskSnapshot>,
    pub usage: ThreadUsageSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolPauseRequest {
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_tool_use_id: Option<String>,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_source: Option<PermissionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
    pub kind: ToolPauseKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSource {
    pub decision: String,
    pub source: String,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolPauseKind {
    Permission(PermissionPreview),
    UserInput(UserInputPreview),
}

impl Serialize for ToolPauseKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct PermissionPayload<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            preview: &'a PermissionPreview,
        }

        #[derive(Serialize)]
        struct UserInputPayload<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            questions: &'a [UserInputQuestion],
        }

        match self {
            Self::Permission(preview) => PermissionPayload {
                kind: "permission",
                preview,
            }
            .serialize(serializer),
            Self::UserInput(preview) => UserInputPayload {
                kind: "user_input",
                questions: &preview.questions,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ToolPauseKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("missing tool pause kind type"))?;

        match kind {
            "permission" => {
                let preview = value.get("preview").ok_or_else(|| {
                    serde::de::Error::custom("permission tool pause missing preview")
                })?;
                serde_json::from_value(preview.clone())
                    .map(Self::Permission)
                    .map_err(serde::de::Error::custom)
            }
            "user_input" => serde_json::from_value(value)
                .map(Self::UserInput)
                .map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "unknown tool pause kind type '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPauseResponse {
    Permission {
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    UserInput {
        value: Value,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionPreview {
    Bash(BashPermissionPreview),
    Edit(EditPermissionPreview),
    Write(EditPermissionPreview),
    Read(ReadPermissionPreview),
    Search(SearchPermissionPreview),
    Mcp(McpPermissionPreview),
    Custom {
        tool_name: String,
        payload: serde_json::Map<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashPermissionPreview {
    pub command: String,
    pub description: Option<String>,
    pub workdir: Option<String>,
    pub timeout: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadPermissionPreview {
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPermissionPreview {
    pub query: String,
    pub mode: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpPermissionPreview {
    pub server_name: String,
    pub server_tool_name: String,
    pub registered_tool_name: String,
    pub inputs: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPermissionPreview {
    pub summary: String,
    pub path: String,
    pub replacement_count: usize,
    /// unified diff 文本(由 core 在 prepare / execute 阶段填入,TUI 用它来还原行号并渲染)。
    /// prepare 和 execute 阶段产出同一份字节,权限弹窗与消息区永远一致。
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputPreview {
    pub questions: Vec<UserInputQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}
