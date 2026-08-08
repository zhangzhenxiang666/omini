use chrono::{DateTime, Utc};
use omini_domain::display::{DisplayMessage, DisplayPlan, DisplaySummary};
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
    RecordParentSubagentUsage {
        thread_id: String,
        usage: Usage,
    },
}
