use super::*;

#[derive(Debug, Clone, FromRow)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub storage_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_opened_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: String,
    pub project_id: String,
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

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub thread_id: String,
    pub role: String,
    pub model_ref: Option<String>,
    pub content: String,
    pub kind: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AgentTask {
    pub task_id: String,
    pub owner_thread_id: String,
    pub agent_thread_id: String,
    pub parent_task_id: Option<String>,
    pub parent_thread_id: String,
    pub spawn_tool_use_id: String,
    pub depth: u8,
    pub execution_mode: AgentTaskExecutionMode,
    pub status: AgentTaskStatus,
    pub agent_name: String,
    pub title: String,
    pub result: Option<AgentTaskResult>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub notification_delivered: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct AgentTaskRow {
    task_id: String,
    owner_thread_id: String,
    agent_thread_id: String,
    parent_task_id: Option<String>,
    parent_thread_id: String,
    spawn_tool_use_id: String,
    depth: i64,
    execution_mode: String,
    status: String,
    agent_name: String,
    title: String,
    result_json: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    notification_delivered: bool,
}

pub struct NewMessage {
    pub thread_id: String,
    pub role: String,
    pub model_ref: Option<String>,
    pub blocks: Vec<ContentBlock>,
    pub kind: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, FromRow)]
pub struct ThreadRow {
    id: String,
    project_id: String,
    parent_thread_id: Option<String>,
    spawn_tool_use_id: Option<String>,
    thread_type: String,
    agent_label: Option<String>,
    provider: String,
    model: String,
    thinking_effort: Option<String>,
    title: Option<String>,
    current_context_tokens: i64,
    total_tokens: i64,
    total_cached_tokens: i64,
    llm_context_version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct StoredMessageRow {
    pub id: i64,
    pub thread_id: String,
    pub role: String,
    pub model_ref: Option<String>,
    pub content: String,
    pub kind: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct StoredLlmMessageRow {
    pub role: String,
    pub content: String,
}

impl From<ThreadRow> for Thread {
    fn from(row: ThreadRow) -> Self {
        Self {
            id: row.id,
            project_id: row.project_id,
            parent_thread_id: row.parent_thread_id,
            spawn_tool_use_id: row.spawn_tool_use_id,
            thread_type: row.thread_type,
            agent_label: row.agent_label,
            provider: row.provider,
            model: row.model,
            thinking_effort: row.thinking_effort,
            title: row.title,
            current_context_tokens: row.current_context_tokens,
            total_tokens: row.total_tokens,
            total_cached_tokens: row.total_cached_tokens,
            llm_context_version: row.llm_context_version,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<StoredMessageRow> for StoredMessage {
    fn from(row: StoredMessageRow) -> Self {
        Self {
            id: row.id,
            thread_id: row.thread_id,
            role: row.role,
            model_ref: row.model_ref,
            content: row.content,
            kind: row.kind,
            created_at: row.created_at,
        }
    }
}

impl TryFrom<AgentTaskRow> for AgentTask {
    type Error = StoreError;

    fn try_from(row: AgentTaskRow) -> Result<Self, Self::Error> {
        let execution_mode = match row.execution_mode.as_str() {
            "background" => AgentTaskExecutionMode::Background,
            "synchronous" => AgentTaskExecutionMode::Synchronous,
            value => {
                return Err(StoreError::InvalidData(format!(
                    "unknown agent task execution mode '{value}'"
                )));
            }
        };
        let status = parse_agent_task_status(&row.status)?;
        let result = row
            .result_json
            .map(|value| serde_json::from_str(&value))
            .transpose()?;
        Ok(Self {
            task_id: row.task_id,
            owner_thread_id: row.owner_thread_id,
            agent_thread_id: row.agent_thread_id,
            parent_task_id: row.parent_task_id,
            parent_thread_id: row.parent_thread_id,
            spawn_tool_use_id: row.spawn_tool_use_id,
            depth: u8::try_from(row.depth).map_err(|_| {
                StoreError::InvalidData(format!("invalid agent task depth {}", row.depth))
            })?,
            execution_mode,
            status,
            agent_name: row.agent_name,
            title: row.title,
            result,
            created_at: row.created_at,
            updated_at: row.updated_at,
            completed_at: row.completed_at,
            notification_delivered: row.notification_delivered,
        })
    }
}

fn parse_agent_task_status(value: &str) -> Result<AgentTaskStatus, StoreError> {
    match value {
        "running" => Ok(AgentTaskStatus::Running),
        "cancelling" => Ok(AgentTaskStatus::Cancelling),
        "completed" => Ok(AgentTaskStatus::Completed),
        "failed" => Ok(AgentTaskStatus::Failed),
        "cancelled" => Ok(AgentTaskStatus::Cancelled),
        "interrupted" => Ok(AgentTaskStatus::Interrupted),
        _ => Err(StoreError::InvalidData(format!(
            "unknown agent task status '{value}'"
        ))),
    }
}
