use chrono::{DateTime, Utc};
use omini_domain::display::{DisplayMessage, DisplayPlan, DisplaySummary};
use omini_domain::message::ContentBlock;
use omini_domain::usage::Usage;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub project_path: String,
    pub parent_session_id: Option<String>,
    pub spawn_tool_use_id: Option<String>,
    pub session_type: String,
    pub agent_label: Option<String>,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<String>,
    pub title: Option<String>,
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum RuntimePersistenceEvent {
    CreateSession(SessionRecord),
    UpdateSessionUpdatedAt {
        session_id: String,
    },
    UpdateSessionConfig {
        session_id: String,
        provider: String,
        model: String,
        thinking_effort: Option<String>,
    },
    UpdateSessionThinkingEffort {
        session_id: String,
        thinking_effort: Option<String>,
    },
    InsertMessage {
        session_id: String,
        role: String,
        blocks: Vec<ContentBlock>,
        kind: String,
        created_at: DateTime<Utc>,
        blocks_dir: PathBuf,
    },
    InsertDisplayMessage {
        session_id: String,
        display: DisplayMessage,
        created_at: DateTime<Utc>,
    },
    InsertPlanMessage {
        session_id: String,
        plan: DisplayPlan,
    },
    InsertCompactSummaryMessage {
        session_id: String,
        summary: DisplaySummary,
    },
    RecordSessionUsage {
        session_id: String,
        usage: Usage,
    },
    RecordSessionTotalUsage {
        session_id: String,
        usage: Usage,
    },
    RecordParentSubagentUsage {
        session_id: String,
        usage: Usage,
    },
}
