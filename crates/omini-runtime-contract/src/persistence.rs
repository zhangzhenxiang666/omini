use chrono::{DateTime, Utc};
use omini_domain::display::{AgentTaskNotification, DisplayMessage, DisplayPlan, DisplaySummary};
use omini_domain::events::{AgentTaskInfo, AgentTaskResult, AgentTaskStatus};
use omini_domain::message::{ContentBlock, Message};
use omini_domain::usage::Usage;
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct ThreadRecord {
    pub id: String,
    pub parent_thread_id: Option<String>,
    pub spawn_tool_use_id: Option<String>,
    pub thread_type: String,
    pub agent_label: Option<String>,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<String>,
    pub title: Option<String>,
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    pub llm_context_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum RuntimePersistenceEvent {
    CreateThread(ThreadRecord),
    /// 原子创建子线程、task 记录和初始用户消息。
    CreateAgentTask {
        task: Box<AgentTaskInfo>,
        thread: ThreadRecord,
        initial_message: Message,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// 在发送 `message_committed` 流式事件前持久化子线程消息。
    PersistAgentMessage {
        thread_id: String,
        message: Message,
        model_ref: Option<String>,
        persist_llm_history: bool,
        display_in_ui: bool,
        created_at: DateTime<Utc>,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// 持久化通道严格有序，因此只有全部子线程消息处理完后才会提交终态。
    FinishAgentTask {
        task_id: String,
        status: AgentTaskStatus,
        result: AgentTaskResult,
        completed_at: DateTime<Utc>,
        ack: oneshot::Sender<Result<(), String>>,
    },
    SetAgentTasksCancelling {
        task_ids: Vec<String>,
        updated_at: DateTime<Utc>,
    },
    InsertAgentTaskNotification {
        owner_thread_id: String,
        notification: AgentTaskNotification,
        llm_message: Message,
        task_ids: Vec<String>,
        created_at: DateTime<Utc>,
        ack: oneshot::Sender<Result<(), String>>,
    },
    UpdateThreadUpdatedAt {
        thread_id: String,
    },
    UpdateThreadConfig {
        thread_id: String,
        provider: String,
        model: String,
        thinking_effort: Option<String>,
    },
    UpdateThreadThinkingEffort {
        thread_id: String,
        thinking_effort: Option<String>,
    },
    InsertMessage {
        thread_id: String,
        role: String,
        model_ref: Option<String>,
        blocks: Vec<ContentBlock>,
        kind: String,
        created_at: DateTime<Utc>,
    },
    InsertDisplayMessage {
        thread_id: String,
        display: DisplayMessage,
        model_ref: Option<String>,
        created_at: DateTime<Utc>,
    },
    InsertPlanMessage {
        thread_id: String,
        plan: DisplayPlan,
        model_ref: String,
    },
    InsertCompactSummaryMessage {
        thread_id: String,
        summary: DisplaySummary,
        model_ref: String,
    },
    AppendLlmMessage {
        thread_id: String,
        message: Message,
        created_at: DateTime<Utc>,
    },
    ReplaceLlmContext {
        thread_id: String,
        expected_version: i64,
        messages: Vec<Message>,
        created_at: DateTime<Utc>,
        ack: oneshot::Sender<Result<i64, String>>,
    },
    RecordThreadUsage {
        thread_id: String,
        usage: Usage,
    },
    RecordThreadTotalUsage {
        thread_id: String,
        usage: Usage,
    },
    RecordOwnerAgentUsage {
        thread_id: String,
        usage: Usage,
    },
}
