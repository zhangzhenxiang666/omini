use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::{DateTime, Utc};
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::display::{DisplayMessage, DisplayPlan, DisplaySummary};
use omini_domain::events::{
    AgentTaskExecutionMode, AgentTaskInfo, AgentTaskResult, AgentTaskStatus,
};
use omini_domain::message::{ContentBlock, Message, Role};
use omini_domain::usage::Usage;
use omini_runtime_contract::persistence::{RuntimePersistenceEvent, ThreadRecord};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

const CONTENT_SIZE_THRESHOLD: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("base64 error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid persisted data: {0}")]
    InvalidData(String),
    #[error("LLM context version conflict: expected {expected}, found {actual}")]
    ContextVersionConflict { expected: i64, actual: i64 },
}

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
struct AgentTaskRow {
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

struct AgentMessagePersistence<'a> {
    thread_id: &'a str,
    message: &'a Message,
    model_ref: Option<&'a str>,
    persist_llm_history: bool,
    display_in_ui: bool,
    created_at: DateTime<Utc>,
    project: &'a ProjectDir,
}

#[derive(Debug, Clone, FromRow)]
struct ThreadRow {
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
struct StoredMessageRow {
    id: i64,
    thread_id: String,
    role: String,
    model_ref: Option<String>,
    content: String,
    kind: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
struct StoredLlmMessageRow {
    role: String,
    content: String,
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

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await?;

        let db = Self { pool };
        db.initialize().await?;
        Ok(db)
    }

    async fn initialize(&self) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS project (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                path            TEXT NOT NULL UNIQUE,
                storage_key     TEXT NOT NULL UNIQUE,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                last_opened_at  TEXT
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS thread (
                id                     TEXT PRIMARY KEY,
                project_id             TEXT NOT NULL REFERENCES project(id) ON DELETE RESTRICT,
                parent_thread_id       TEXT REFERENCES thread(id) ON DELETE CASCADE,
                spawn_tool_use_id      TEXT,
                thread_type            TEXT NOT NULL DEFAULT 'main',
                agent_label            TEXT,
                provider               TEXT NOT NULL,
                model                  TEXT NOT NULL,
                thinking_effort        TEXT,
                title                  TEXT,
                current_context_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens           INTEGER NOT NULL DEFAULT 0,
                total_cached_tokens    INTEGER NOT NULL DEFAULT 0,
                llm_context_version    INTEGER NOT NULL DEFAULT 0,
                created_at             TEXT NOT NULL,
                updated_at             TEXT NOT NULL
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id       TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                role            TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
                model_ref       TEXT,
                content         TEXT NOT NULL,
                kind            TEXT NOT NULL DEFAULT 'normal',
                created_at      TEXT NOT NULL,
                CHECK (
                    (role = 'assistant' AND model_ref IS NOT NULL) OR
                    (role <> 'assistant' AND model_ref IS NULL)
                )
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS llm_messages (
                thread_id          TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                context_version    INTEGER NOT NULL,
                ordinal            INTEGER NOT NULL,
                role               TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
                content            TEXT NOT NULL,
                created_at         TEXT NOT NULL,
                PRIMARY KEY (thread_id, context_version, ordinal)
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_task (
                task_id                 TEXT PRIMARY KEY,
                owner_thread_id         TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                agent_thread_id         TEXT NOT NULL UNIQUE REFERENCES thread(id) ON DELETE CASCADE,
                parent_task_id          TEXT REFERENCES agent_task(task_id) ON DELETE CASCADE,
                parent_thread_id        TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                spawn_tool_use_id       TEXT NOT NULL,
                depth                   INTEGER NOT NULL,
                execution_mode          TEXT NOT NULL CHECK (execution_mode IN ('background', 'synchronous')),
                status                  TEXT NOT NULL CHECK (status IN ('running', 'cancelling', 'completed', 'failed', 'cancelled', 'interrupted')),
                agent_name              TEXT NOT NULL,
                title                   TEXT NOT NULL,
                result_json             TEXT,
                created_at              TEXT NOT NULL,
                updated_at              TEXT NOT NULL,
                completed_at            TEXT,
                notification_delivered  INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_thread_project ON thread(project_id)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_thread_parent ON thread(parent_thread_id)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id, id)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_llm_messages_current ON llm_messages(thread_id, context_version, ordinal)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_agent_task_owner ON agent_task(owner_thread_id, created_at)",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE agent_task SET status = 'interrupted', completed_at = COALESCE(completed_at, ?), updated_at = ? WHERE status = 'running'",
        )
        .bind(Utc::now())
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_task SET status = 'cancelled', completed_at = COALESCE(completed_at, ?), updated_at = ? WHERE status = 'cancelling'",
        )
        .bind(Utc::now())
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn create_project(&self, project: &Project) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO project(
                    id, 
                    name, 
                    path, 
                    storage_key, 
                    created_at, 
                    updated_at, 
                    last_opened_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&project.id)
        .bind(&project.name)
        .bind(&project.path)
        .bind(&project.storage_key)
        .bind(project.created_at)
        .bind(project.updated_at)
        .bind(project.last_opened_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_project(&self, id: &str) -> Result<Option<Project>, StoreError> {
        Ok(
            sqlx::query_as::<_, Project>("SELECT * FROM project WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn get_project_by_path(&self, path: &str) -> Result<Option<Project>, StoreError> {
        Ok(
            sqlx::query_as::<_, Project>("SELECT * FROM project WHERE path = ?")
                .bind(path)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, StoreError> {
        Ok(sqlx::query_as::<_, Project>(
            "SELECT * 
                FROM project
                ORDER BY last_opened_at IS NULL, last_opened_at DESC, created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn update_project(&self, project: &Project) -> Result<(), StoreError> {
        sqlx::query("UPDATE project SET name = ?, path = ?, updated_at = ? WHERE id = ?")
            .bind(&project.name)
            .bind(&project.path)
            .bind(project.updated_at)
            .bind(&project.id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_project_opened(&self, id: &str) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query("UPDATE project SET last_opened_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_thread(&self, thread: &Thread) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO thread(
                    id,
                    project_id,
                    parent_thread_id,
                    spawn_tool_use_id,
                    thread_type,
                    agent_label,
                    provider,
                    model,
                    thinking_effort,
                    title,
                    current_context_tokens,
                    total_tokens,
                    total_cached_tokens,
                    llm_context_version,
                    created_at,
                    updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&thread.id)
        .bind(&thread.project_id)
        .bind(&thread.parent_thread_id)
        .bind(&thread.spawn_tool_use_id)
        .bind(&thread.thread_type)
        .bind(&thread.agent_label)
        .bind(&thread.provider)
        .bind(&thread.model)
        .bind(&thread.thinking_effort)
        .bind(&thread.title)
        .bind(thread.current_context_tokens)
        .bind(thread.total_tokens)
        .bind(thread.total_cached_tokens)
        .bind(thread.llm_context_version)
        .bind(thread.created_at)
        .bind(thread.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_agent_task(
        &self,
        project_id: &str,
        task: &AgentTaskInfo,
        thread: &ThreadRecord,
        initial_message: &Message,
    ) -> Result<(), StoreError> {
        let thread = thread_from_runtime(project_id, thread);
        let initial_content = serde_json::to_string(&initial_message.content)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO thread(
                    id,
                    project_id,
                    parent_thread_id,
                    spawn_tool_use_id,
                    thread_type,
                    agent_label,
                    provider,
                    model,
                    thinking_effort,
                    title,
                    current_context_tokens,
                    total_tokens,
                    total_cached_tokens,
                    llm_context_version,
                    created_at,
                    updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&thread.id)
        .bind(&thread.project_id)
        .bind(&thread.parent_thread_id)
        .bind(&thread.spawn_tool_use_id)
        .bind(&thread.thread_type)
        .bind(&thread.agent_label)
        .bind(&thread.provider)
        .bind(&thread.model)
        .bind(&thread.thinking_effort)
        .bind(&thread.title)
        .bind(thread.current_context_tokens)
        .bind(thread.total_tokens)
        .bind(thread.total_cached_tokens)
        .bind(thread.llm_context_version)
        .bind(thread.created_at)
        .bind(thread.updated_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO agent_task(
                    task_id,
                    owner_thread_id,
                    agent_thread_id,
                    parent_task_id,
                    parent_thread_id,
                    spawn_tool_use_id,
                    depth,
                    execution_mode,
                    status,
                    agent_name,
                    title,
                    result_json,
                    created_at,
                    updated_at,
                    completed_at,
                    notification_delivered
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, 0)",
        )
        .bind(&task.task_id)
        .bind(&task.owner_thread_id)
        .bind(&task.thread_id)
        .bind(&task.parent_task_id)
        .bind(&task.parent_thread_id)
        .bind(&task.spawn_tool_use_id)
        .bind(i64::from(task.depth))
        .bind(task.execution_mode.as_str())
        .bind(task.status.as_str())
        .bind(&task.agent)
        .bind(&task.title)
        .bind(task.created_at)
        .bind(task.updated_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO messages(
                    thread_id,
                    role,
                    model_ref,
                    content,
                    kind,
                    created_at
                )
                VALUES (?, 'user', NULL, ?, 'normal', ?)",
        )
        .bind(&task.thread_id)
        .bind(&initial_content)
        .bind(task.created_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO llm_messages(
                    thread_id,
                    context_version,
                    ordinal,
                    role,
                    content,
                    created_at)
                VALUES (?, 1, 0, 'user', ?, ?)",
        )
        .bind(&task.thread_id)
        .bind(initial_content)
        .bind(task.created_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_agent_tasks(
        &self,
        owner_thread_id: &str,
    ) -> Result<Vec<AgentTask>, StoreError> {
        let rows = sqlx::query_as::<_, AgentTaskRow>(
            "SELECT * FROM agent_task WHERE owner_thread_id = ? ORDER BY created_at, task_id",
        )
        .bind(owner_thread_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn persist_agent_message(
        &self,
        request: AgentMessagePersistence<'_>,
    ) -> Result<(), StoreError> {
        if request.display_in_ui {
            self.insert_message(
                &NewMessage {
                    thread_id: request.thread_id.to_string(),
                    role: request.message.role.to_string(),
                    model_ref: (request.message.role == Role::Assistant)
                        .then(|| request.model_ref.map(str::to_string))
                        .flatten(),
                    blocks: request.message.content.clone(),
                    kind: "normal".to_string(),
                    created_at: request.created_at,
                },
                &request.project.thread(request.thread_id),
            )
            .await?;
        }
        if request.persist_llm_history {
            self.append_llm_message(
                request.thread_id,
                request.message,
                request.created_at,
                &request.project.thread(request.thread_id),
            )
            .await?;
        }
        Ok(())
    }

    async fn finish_agent_task(
        &self,
        task_id: &str,
        status: AgentTaskStatus,
        result: &AgentTaskResult,
        completed_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE agent_task SET
                    status = ?,
                    result_json = ?,
                    updated_at = ?,
                    completed_at = ?
                WHERE task_id = ?",
        )
        .bind(status.as_str())
        .bind(serde_json::to_string(result)?)
        .bind(completed_at)
        .bind(completed_at)
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_agent_tasks_cancelling(
        &self,
        task_ids: &[String],
        updated_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        for task_id in task_ids {
            sqlx::query(
                "UPDATE agent_task SET
                status = 'cancelling',
                updated_at = ?
                WHERE task_id = ? AND status = 'running'",
            )
            .bind(updated_at)
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn insert_agent_task_notification(
        &self,
        owner_thread_id: &str,
        notification: &omini_domain::display::AgentTaskNotification,
        llm_message: &Message,
        task_ids: &[String],
        created_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let mut has_pending_task = false;
        for task_id in task_ids {
            let pending: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM agent_task
                    WHERE task_id = ? AND notification_delivered = 0
                )",
            )
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await?;
            has_pending_task |= pending;
        }
        if !has_pending_task {
            tx.commit().await?;
            return Ok(());
        }

        let notification_json = serde_json::to_string(notification)?;
        let llm_json = serde_json::to_string(&llm_message.content)?;
        sqlx::query(
            "INSERT INTO messages(
                    thread_id,
                    role,
                    model_ref,
                    content,
                    kind,
                    created_at
                )
                VALUES (?, 'user', NULL, ?, 'agent_task_notification', ?)",
        )
        .bind(owner_thread_id)
        .bind(notification_json)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
        let version: i64 =
            sqlx::query_scalar("SELECT llm_context_version FROM thread WHERE id = ?")
                .bind(owner_thread_id)
                .fetch_one(&mut *tx)
                .await?;
        let ordinal: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM llm_messages WHERE thread_id = ? AND context_version = ?",
        )
        .bind(owner_thread_id)
        .bind(version)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO llm_messages(
                    thread_id,
                    context_version,
                    ordinal,
                    role,
                    content,
                    created_at
                )
                VALUES (?, ?, ?, 'user', ?, ?)",
        )
        .bind(owner_thread_id)
        .bind(version)
        .bind(ordinal)
        .bind(llm_json)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
        for task_id in task_ids {
            sqlx::query(
                "UPDATE agent_task SET
                        notification_delivered = 1,
                        updated_at = ?
                    WHERE task_id = ? AND notification_delivered = 0",
            )
            .bind(created_at)
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_thread(&self, id: &str) -> Result<Option<Thread>, StoreError> {
        let row = sqlx::query_as::<_, ThreadRow>("SELECT * FROM thread WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(Into::into))
    }

    pub async fn list_threads(&self, project_id: &str) -> Result<Vec<Thread>, StoreError> {
        let rows = sqlx::query_as::<_, ThreadRow>(
            "SELECT * FROM thread
                WHERE project_id = ? AND thread_type = 'main'
                ORDER BY updated_at DESC, created_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn list_child_threads(&self, parent_id: &str) -> Result<Vec<Thread>, StoreError> {
        let rows = sqlx::query_as::<_, ThreadRow>(
            "SELECT * FROM thread WHERE parent_thread_id = ? ORDER BY created_at ASC",
        )
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn record_thread_usage(&self, id: &str, usage: Usage) -> Result<(), StoreError> {
        let now = Utc::now();
        let total_tokens = usage_tokens_i64(usage);
        let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
        sqlx::query(
            "UPDATE thread SET
                    current_context_tokens = ?,
                    total_tokens = total_tokens + ?,
                    total_cached_tokens = total_cached_tokens + ?,
                    updated_at = ?
                WHERE id = ?",
        )
        .bind(total_tokens)
        .bind(total_tokens)
        .bind(cached_tokens)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_thread_total_usage(
        &self,
        id: &str,
        usage: Usage,
    ) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE thread SET
                    total_tokens = total_tokens + ?,
                    total_cached_tokens = total_cached_tokens + ?,
                    updated_at = ?
                WHERE id = ?",
        )
        .bind(usage_tokens_i64(usage))
        .bind(usage_usize_to_i64(usage.cached_tokens))
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_thread_updated_at(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE thread SET updated_at = ? WHERE id = ?")
            .bind(Utc::now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_thread_config(
        &self,
        id: &str,
        provider: &str,
        model: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE thread SET
                    provider = ?,
                    model = ?,
                    thinking_effort = ?,
                    updated_at = ?
                WHERE id = ?",
        )
        .bind(provider)
        .bind(model)
        .bind(thinking_effort)
        .bind(Utc::now())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_thread_thinking_effort(
        &self,
        id: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE thread SET thinking_effort = ?, updated_at = ? WHERE id = ?")
            .bind(thinking_effort)
            .bind(Utc::now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_thread_title(&self, id: &str, title: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE thread SET title = ?, updated_at = ? WHERE id = ?")
            .bind(title)
            .bind(Utc::now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_initial_thread_title(
        &self,
        id: &str,
        title: &str,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "UPDATE thread SET
                    title = ?,
                    updated_at = ?
                WHERE id = ?
                AND (title IS NULL OR TRIM(title) = '')
                AND NOT EXISTS (SELECT 1 FROM messages WHERE thread_id = ? LIMIT 1)",
        )
        .bind(title)
        .bind(Utc::now())
        .bind(id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn insert_message(
        &self,
        msg: &NewMessage,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let prepared = prepare_blocks(&msg.blocks, thread_dir)?;
        let blocks_json = serde_json::to_string(&prepared.values)?;
        let result = sqlx::query(
            "INSERT INTO messages(
                    thread_id,
                    role,
                    model_ref,
                    content,
                    kind,
                    created_at
                )
                VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&msg.thread_id)
        .bind(&msg.role)
        .bind(&msg.model_ref)
        .bind(blocks_json)
        .bind(&msg.kind)
        .bind(msg.created_at)
        .execute(&self.pool)
        .await;
        finish_prepared_write(result.map(|_| ()), &prepared.created_files)
    }

    async fn insert_display_message(
        &self,
        thread_id: &str,
        display: &DisplayMessage,
        model_ref: Option<&str>,
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: &display.role.to_string(),
                model_ref,
                content: &serde_json::to_string(display)?,
                kind: "display",
                created_at,
            },
            thread_dir,
        )
        .await
    }

    async fn insert_plan_message(
        &self,
        thread_id: &str,
        plan: &DisplayPlan,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: "assistant",
                model_ref: Some(model_ref),
                content: &serde_json::to_string(plan)?,
                kind: "plan",
                created_at: plan.created_at,
            },
            thread_dir,
        )
        .await
    }

    async fn insert_compact_summary_message(
        &self,
        thread_id: &str,
        summary: &DisplaySummary,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: "assistant",
                model_ref: Some(model_ref),
                content: &serde_json::to_string(summary)?,
                kind: "compact_summary",
                created_at: summary.created_at,
            },
            thread_dir,
        )
        .await
    }

    pub async fn get_messages(&self, thread_id: &str) -> Result<Vec<StoredMessage>, StoreError> {
        let rows = sqlx::query_as::<_, StoredMessageRow>(
            "SELECT * FROM messages WHERE thread_id = ? ORDER BY id",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn get_first_message_text(&self, thread_id: &str) -> Result<String, StoreError> {
        let row = sqlx::query_as::<_, StoredMessageRow>(
            "SELECT * FROM messages WHERE thread_id = ? ORDER BY id ASC LIMIT 1",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .map(|message| extract_message_text(&message.content))
            .unwrap_or_default())
    }

    pub async fn append_llm_message(
        &self,
        thread_id: &str,
        message: &Message,
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let prepared = prepare_blocks(&message.content, thread_dir)?;
        let content = serde_json::to_string(&prepared.values)?;
        let mut tx = self.pool.begin().await?;
        let version: i64 =
            sqlx::query_scalar("SELECT llm_context_version FROM thread WHERE id = ?")
                .bind(thread_id)
                .fetch_one(&mut *tx)
                .await?;
        let ordinal: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(ordinal) + 1, 0)
                FROM llm_messages
                WHERE thread_id = ? AND context_version = ?",
        )
        .bind(thread_id)
        .bind(version)
        .fetch_one(&mut *tx)
        .await?;
        let result = sqlx::query(
            "INSERT INTO llm_messages(
                    thread_id,
                    context_version,
                    ordinal,
                    role,
                    content,
                    created_at
                )
                VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(thread_id)
        .bind(version)
        .bind(ordinal)
        .bind(message.role.to_string())
        .bind(content)
        .bind(created_at)
        .execute(&mut *tx)
        .await;
        if let Err(error) = result {
            cleanup_created_files(&prepared.created_files);
            return Err(error.into());
        }
        if let Err(error) = tx.commit().await {
            cleanup_created_files(&prepared.created_files);
            return Err(error.into());
        }
        Ok(())
    }

    pub async fn replace_llm_context(
        &self,
        thread_id: &str,
        expected_version: i64,
        messages: &[Message],
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<i64, StoreError> {
        let mut prepared_messages = Vec::with_capacity(messages.len());
        let mut created_files = Vec::new();
        for message in messages {
            match prepare_blocks(&message.content, thread_dir) {
                Ok(prepared) => {
                    created_files.extend(prepared.created_files);
                    prepared_messages.push((message.role.to_string(), prepared.values));
                }
                Err(error) => {
                    cleanup_created_files(&created_files);
                    return Err(error);
                }
            }
        }

        let result = async {
            let mut tx = self.pool.begin().await?;
            let actual: i64 =
                sqlx::query_scalar("SELECT llm_context_version FROM thread WHERE id = ?")
                    .bind(thread_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if actual != expected_version {
                return Err(StoreError::ContextVersionConflict {
                    expected: expected_version,
                    actual,
                });
            }
            let next_version = expected_version + 1;
            for (ordinal, (role, blocks)) in prepared_messages.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO llm_messages(
                            thread_id,
                            context_version,
                            ordinal,
                            role,
                            content,
                            created_at
                        )
                        VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(thread_id)
                .bind(next_version)
                .bind(i64::try_from(ordinal).unwrap_or(i64::MAX))
                .bind(role)
                .bind(serde_json::to_string(blocks)?)
                .bind(created_at)
                .execute(&mut *tx)
                .await?;
            }
            let updated = sqlx::query(
                "UPDATE thread SET
                        llm_context_version = ?,
                        updated_at = ?
                    WHERE id = ? AND llm_context_version = ?",
            )
            .bind(next_version)
            .bind(Utc::now())
            .bind(thread_id)
            .bind(expected_version)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() != 1 {
                return Err(StoreError::ContextVersionConflict {
                    expected: expected_version,
                    actual,
                });
            }
            tx.commit().await?;
            Ok(next_version)
        }
        .await;

        if result.is_err() {
            cleanup_created_files(&created_files);
        }
        result
    }

    pub async fn load_current_llm_messages(
        &self,
        thread_id: &str,
        thread_dir: &ThreadDir,
    ) -> Result<Vec<Message>, StoreError> {
        let rows = sqlx::query_as::<_, StoredLlmMessageRow>(
            "SELECT lm.role, lm.content
                FROM llm_messages lm
                JOIN thread t ON t.id = lm.thread_id
                WHERE lm.thread_id = ? AND lm.context_version = t.llm_context_version
                ORDER BY lm.ordinal",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let role = parse_role(&row.role)?;
                let stored = serde_json::from_str::<Vec<serde_json::Value>>(&row.content)?;
                Ok(Message::new(role, load_blocks(&stored, thread_dir)?))
            })
            .collect()
    }

    pub async fn delete_thread_tree(
        &self,
        thread_id: &str,
        project: &ProjectDir,
    ) -> Result<(), StoreError> {
        let ids = sqlx::query_scalar::<_, String>(
            "WITH RECURSIVE descendants(id) AS (
                    SELECT id FROM thread WHERE id = ?
                    UNION ALL
                    SELECT child.id FROM thread child
                    JOIN descendants parent ON child.parent_thread_id = parent.id
                ) SELECT id FROM descendants",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        sqlx::query("DELETE FROM thread WHERE id = ?")
            .bind(thread_id)
            .execute(&self.pool)
            .await?;
        for id in ids {
            let path = project.thread(&id).path().to_path_buf();
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
        Ok(())
    }

    pub async fn apply_persistence_event(
        &self,
        event: &RuntimePersistenceEvent,
        project_id: &str,
        project: &ProjectDir,
    ) -> Result<(), StoreError> {
        match event {
            RuntimePersistenceEvent::CreateThread(thread) => {
                self.create_thread(&thread_from_runtime(project_id, thread))
                    .await
            }
            RuntimePersistenceEvent::CreateAgentTask {
                task,
                thread,
                initial_message,
                ..
            } => {
                self.create_agent_task(project_id, task, thread, initial_message)
                    .await
            }
            RuntimePersistenceEvent::PersistAgentMessage {
                thread_id,
                message,
                model_ref,
                persist_llm_history,
                display_in_ui,
                created_at,
                ..
            } => {
                self.persist_agent_message(AgentMessagePersistence {
                    thread_id,
                    message,
                    model_ref: model_ref.as_deref(),
                    persist_llm_history: *persist_llm_history,
                    display_in_ui: *display_in_ui,
                    created_at: *created_at,
                    project,
                })
                .await
            }
            RuntimePersistenceEvent::FinishAgentTask {
                task_id,
                status,
                result,
                completed_at,
                ..
            } => {
                self.finish_agent_task(task_id, *status, result, *completed_at)
                    .await
            }
            RuntimePersistenceEvent::SetAgentTasksCancelling {
                task_ids,
                updated_at,
            } => self.set_agent_tasks_cancelling(task_ids, *updated_at).await,
            RuntimePersistenceEvent::InsertAgentTaskNotification {
                owner_thread_id,
                notification,
                llm_message,
                task_ids,
                created_at,
                ..
            } => {
                self.insert_agent_task_notification(
                    owner_thread_id,
                    notification,
                    llm_message,
                    task_ids,
                    *created_at,
                )
                .await
            }
            RuntimePersistenceEvent::UpdateThreadUpdatedAt { thread_id } => {
                self.update_thread_updated_at(thread_id).await
            }
            RuntimePersistenceEvent::UpdateThreadConfig {
                thread_id,
                provider,
                model,
                thinking_effort,
            } => {
                self.update_thread_config(thread_id, provider, model, thinking_effort.as_deref())
                    .await
            }
            RuntimePersistenceEvent::UpdateThreadThinkingEffort {
                thread_id,
                thinking_effort,
            } => {
                self.update_thread_thinking_effort(thread_id, thinking_effort.as_deref())
                    .await
            }
            RuntimePersistenceEvent::InsertMessage {
                thread_id,
                role,
                model_ref,
                blocks,
                kind,
                created_at,
            } => {
                self.insert_message(
                    &NewMessage {
                        thread_id: thread_id.clone(),
                        role: role.clone(),
                        model_ref: model_ref.clone(),
                        blocks: blocks.clone(),
                        kind: kind.clone(),
                        created_at: *created_at,
                    },
                    &project.thread(thread_id),
                )
                .await
            }
            RuntimePersistenceEvent::InsertDisplayMessage {
                thread_id,
                display,
                model_ref,
                created_at,
            } => {
                self.insert_display_message(
                    thread_id,
                    display,
                    model_ref.as_deref(),
                    *created_at,
                    &project.thread(thread_id),
                )
                .await
            }
            RuntimePersistenceEvent::InsertPlanMessage {
                thread_id,
                plan,
                model_ref,
            } => {
                self.insert_plan_message(thread_id, plan, model_ref, &project.thread(thread_id))
                    .await
            }
            RuntimePersistenceEvent::InsertCompactSummaryMessage {
                thread_id,
                summary,
                model_ref,
            } => {
                self.insert_compact_summary_message(
                    thread_id,
                    summary,
                    model_ref,
                    &project.thread(thread_id),
                )
                .await
            }
            RuntimePersistenceEvent::AppendLlmMessage {
                thread_id,
                message,
                created_at,
            } => {
                self.append_llm_message(thread_id, message, *created_at, &project.thread(thread_id))
                    .await
            }
            RuntimePersistenceEvent::ReplaceLlmContext {
                thread_id,
                expected_version,
                messages,
                created_at,
                ..
            } => self
                .replace_llm_context(
                    thread_id,
                    *expected_version,
                    messages,
                    *created_at,
                    &project.thread(thread_id),
                )
                .await
                .map(|_| ()),
            RuntimePersistenceEvent::RecordThreadUsage { thread_id, usage } => {
                self.record_thread_usage(thread_id, *usage).await
            }
            RuntimePersistenceEvent::RecordThreadTotalUsage { thread_id, usage } => {
                self.record_thread_total_usage(thread_id, *usage).await
            }
            RuntimePersistenceEvent::RecordOwnerAgentUsage { thread_id, usage } => {
                self.record_thread_total_usage(thread_id, *usage).await
            }
        }
    }
}

fn thread_from_runtime(project_id: &str, thread: &ThreadRecord) -> Thread {
    Thread {
        id: thread.id.clone(),
        project_id: project_id.to_string(),
        parent_thread_id: thread.parent_thread_id.clone(),
        spawn_tool_use_id: thread.spawn_tool_use_id.clone(),
        thread_type: thread.thread_type.clone(),
        agent_label: thread.agent_label.clone(),
        provider: thread.provider.clone(),
        model: thread.model.clone(),
        thinking_effort: thread.thinking_effort.clone(),
        title: thread.title.clone(),
        current_context_tokens: thread.current_context_tokens,
        total_tokens: thread.total_tokens,
        total_cached_tokens: thread.total_cached_tokens,
        llm_context_version: thread.llm_context_version,
        created_at: thread.created_at,
        updated_at: thread.updated_at,
    }
}

struct NewUiJson<'a> {
    thread_id: &'a str,
    role: &'a str,
    model_ref: Option<&'a str>,
    content: &'a str,
    kind: &'a str,
    created_at: DateTime<Utc>,
}

async fn insert_ui_json(
    pool: &SqlitePool,
    row: NewUiJson<'_>,
    thread_dir: &ThreadDir,
) -> Result<(), StoreError> {
    let PreparedUiContent {
        value,
        created_files,
    } = prepare_ui_content(row.content, thread_dir)?;
    let result = sqlx::query(
        "INSERT INTO messages(
                thread_id,
                role,
                model_ref,
                content,
                kind,
                created_at)
            VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(row.thread_id)
    .bind(row.role)
    .bind(row.model_ref)
    .bind(value)
    .bind(row.kind)
    .bind(row.created_at)
    .execute(pool)
    .await;
    finish_prepared_write(result.map(|_| ()), &created_files)
}

fn usage_tokens_i64(usage: Usage) -> i64 {
    usage_usize_to_i64(usage.total_tokens())
}

fn usage_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) struct PreparedBlocks {
    values: Vec<serde_json::Value>,
    created_files: Vec<PathBuf>,
}

struct PreparedUiContent {
    value: String,
    created_files: Vec<PathBuf>,
}

fn prepare_ui_content(
    content: &str,
    thread_dir: &ThreadDir,
) -> Result<PreparedUiContent, StoreError> {
    if content.len() <= CONTENT_SIZE_THRESHOLD {
        return Ok(PreparedUiContent {
            value: content.to_string(),
            created_files: Vec::new(),
        });
    }
    let bytes = content.as_bytes();
    let sha256 = sha256_hex(bytes);
    let relative_path = PathBuf::from("sidecars").join(format!("{sha256}.json"));
    let path = thread_dir.path().join(&relative_path);
    let created_files = if write_atomically_if_absent(thread_dir, &path, bytes)? {
        vec![path]
    } else {
        Vec::new()
    };
    Ok(PreparedUiContent {
        value: serde_json::to_string(&serde_json::json!({
            "type": "sidecar_document",
            "path": path_to_relative_string(&relative_path)?,
            "bytes": bytes.len(),
            "sha256": sha256,
        }))?,
        created_files,
    })
}

pub(crate) fn load_ui_content(stored: &str, thread_dir: &ThreadDir) -> Result<String, StoreError> {
    let Ok(reference) = serde_json::from_str::<serde_json::Value>(stored) else {
        return Ok(stored.to_string());
    };
    if reference.get("type").and_then(serde_json::Value::as_str) != Some("sidecar_document") {
        return Ok(stored.to_string());
    }
    let relative = required_string(&reference, "path")?;
    let bytes = fs::read(safe_relative_path(thread_dir.path(), relative)?)?;
    verify_stored_bytes(&reference, &bytes)?;
    String::from_utf8(bytes)
        .map_err(|error| StoreError::InvalidData(format!("sidecar is not UTF-8: {error}")))
}

pub(crate) fn prepare_blocks(
    blocks: &[ContentBlock],
    thread_dir: &ThreadDir,
) -> Result<PreparedBlocks, StoreError> {
    let mut values = Vec::with_capacity(blocks.len());
    let mut created_files = Vec::new();
    for block in blocks {
        if let ContentBlock::Image(image) = block {
            let bytes = BASE64_STANDARD.decode(&image.source.data)?;
            let sha256 = sha256_hex(&bytes);
            let relative_path = asset_relative_path(&sha256, &image.source.media_type)?;
            let path = thread_dir.path().join(&relative_path);
            if write_atomically_if_absent(thread_dir, &path, &bytes)? {
                created_files.push(path);
            }
            values.push(serde_json::json!({
                "type": "asset",
                "path": path_to_relative_string(&relative_path)?,
                "mime_type": image.source.media_type,
                "bytes": bytes.len(),
                "sha256": sha256,
            }));
            continue;
        }

        let encoded = serde_json::to_vec(block)?;
        if should_externalize_block(block, encoded.len()) {
            let sha256 = sha256_hex(&encoded);
            let relative_path = PathBuf::from("sidecars").join(format!("{sha256}.json"));
            let path = thread_dir.path().join(&relative_path);
            if write_atomically_if_absent(thread_dir, &path, &encoded)? {
                created_files.push(path);
            }
            values.push(serde_json::json!({
                "type": "sidecar",
                "path": path_to_relative_string(&relative_path)?,
                "bytes": encoded.len(),
                "sha256": sha256,
            }));
        } else {
            values.push(serde_json::from_slice(&encoded)?);
        }
    }
    Ok(PreparedBlocks {
        values,
        created_files,
    })
}

fn should_externalize_block(block: &ContentBlock, encoded_len: usize) -> bool {
    match block {
        ContentBlock::Text(block) => block.text.len() > CONTENT_SIZE_THRESHOLD,
        ContentBlock::Thinking(block) => block.thinking.len() > CONTENT_SIZE_THRESHOLD,
        ContentBlock::ToolResult(block) => block.content.len() > CONTENT_SIZE_THRESHOLD,
        ContentBlock::ToolUse(_) => encoded_len > CONTENT_SIZE_THRESHOLD,
        ContentBlock::Image(_) => false,
    }
}

pub(crate) fn load_blocks(
    stored: &[serde_json::Value],
    thread_dir: &ThreadDir,
) -> Result<Vec<ContentBlock>, StoreError> {
    stored
        .iter()
        .map(
            |value| match value.get("type").and_then(serde_json::Value::as_str) {
                Some("asset") => {
                    let relative = required_string(value, "path")?;
                    let path = safe_relative_path(thread_dir.path(), relative)?;
                    let bytes = fs::read(path)?;
                    verify_stored_bytes(value, &bytes)?;
                    Ok(ContentBlock::from_base64_image(
                        required_string(value, "mime_type")?.to_string(),
                        BASE64_STANDARD.encode(bytes),
                    ))
                }
                Some("sidecar") => {
                    let relative = required_string(value, "path")?;
                    let path = safe_relative_path(thread_dir.path(), relative)?;
                    let bytes = fs::read(path)?;
                    verify_stored_bytes(value, &bytes)?;
                    Ok(serde_json::from_slice(&bytes)?)
                }
                _ => Ok(serde_json::from_value(value.clone())?),
            },
        )
        .collect()
}

pub(crate) fn persist_asset(
    thread_dir: &ThreadDir,
    bytes: &[u8],
    mime_type: &str,
) -> Result<(String, String), StoreError> {
    let sha256 = sha256_hex(bytes);
    let relative_path = asset_relative_path(&sha256, mime_type)?;
    let path = thread_dir.path().join(&relative_path);
    write_atomically_if_absent(thread_dir, &path, bytes)?;
    Ok((sha256, path_to_relative_string(&relative_path)?))
}

pub(crate) fn asset_path(
    thread_dir: &ThreadDir,
    sha256: &str,
    mime_type: &str,
) -> Result<PathBuf, StoreError> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidData("invalid attachment id".to_string()));
    }
    Ok(thread_dir
        .path()
        .join(asset_relative_path(sha256, mime_type)?))
}

fn asset_relative_path(sha256: &str, mime_type: &str) -> Result<PathBuf, StoreError> {
    let extension = match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => {
            return Err(StoreError::InvalidData(format!(
                "unsupported attachment MIME type {mime_type}"
            )));
        }
    };
    Ok(PathBuf::from("assets").join(format!("{sha256}.{extension}")))
}

fn write_atomically_if_absent(
    thread_dir: &ThreadDir,
    destination: &Path,
    bytes: &[u8],
) -> Result<bool, StoreError> {
    if destination.exists() {
        return Ok(false);
    }
    fs::create_dir_all(thread_dir.staging_dir())?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = thread_dir
        .staging_dir()
        .join(format!("{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), std::io::Error> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp_path, destination)?;
        if let Some(parent) = destination.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }
    Ok(true)
}

fn required_string<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, StoreError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| StoreError::InvalidData(format!("missing {field}")))
}

fn verify_stored_bytes(value: &serde_json::Value, bytes: &[u8]) -> Result<(), StoreError> {
    let expected_bytes = value
        .get("bytes")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| StoreError::InvalidData("missing bytes".to_string()))?;
    let expected_hash = required_string(value, "sha256")?;
    if expected_bytes != bytes.len() as u64 || expected_hash != sha256_hex(bytes) {
        return Err(StoreError::InvalidData(
            "sidecar or asset integrity check failed".to_string(),
        ));
    }
    Ok(())
}

fn safe_relative_path(root: &Path, relative: &str) -> Result<PathBuf, StoreError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(StoreError::InvalidData(
            "persisted path is not a safe relative path".to_string(),
        ));
    }
    Ok(root.join(relative))
}

fn path_to_relative_string(path: &Path) -> Result<String, StoreError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| StoreError::InvalidData("non-UTF-8 persisted path".to_string()))
}

// TODO: 需要考虑这里的性能问题
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn finish_prepared_write(
    result: Result<(), sqlx::Error>,
    created_files: &[PathBuf],
) -> Result<(), StoreError> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            cleanup_created_files(created_files);
            Err(error.into())
        }
    }
}

fn cleanup_created_files(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "failed to clean unreferenced file");
        }
    }
}

fn parse_role(role: &str) -> Result<Role, StoreError> {
    match role {
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        _ => Err(StoreError::InvalidData(format!("unknown role {role}"))),
    }
}

fn extract_message_text(content_json: &str) -> String {
    if let Ok(display) = serde_json::from_str::<DisplayMessage>(content_json) {
        return display.text.replace('\n', " ").replace('\r', "");
    }
    if let Ok(summary) = serde_json::from_str::<DisplaySummary>(content_json) {
        return summary.markdown.replace('\n', " ").replace('\r', "");
    }
    serde_json::from_str::<Vec<serde_json::Value>>(content_json)
        .unwrap_or_default()
        .iter()
        .filter(|value| value.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|value| value.get("text").and_then(serde_json::Value::as_str))
        .map(|text| text.replace('\n', " ").replace('\r', ""))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use omini_domain::message::{ImageSource, ImageSourceType, TextBlock};
    use std::sync::atomic::{AtomicU64, Ordering};

    const TEST_PROJECT_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn new() -> Self {
            let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "omini-server-store-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test root should be created");
            Self { path }
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn fixed_time() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 0, 0, 0)
            .single()
            .expect("fixed test time should be valid")
    }

    async fn temp_db() -> (Database, ProjectDir, TestRoot) {
        let root = TestRoot::new();
        let db = Database::open(&root.path.join("omini.sqlite"))
            .await
            .unwrap();
        let now = fixed_time();
        db.create_project(&Project {
            id: TEST_PROJECT_ID.to_string(),
            name: "test project".to_string(),
            path: root.path.join("cwd").display().to_string(),
            storage_key: "test-project".to_string(),
            created_at: now,
            updated_at: now,
            last_opened_at: None,
        })
        .await
        .unwrap();
        (db, ProjectDir::from_path(root.path.join("project")), root)
    }

    fn test_thread(id: &str) -> Thread {
        let now = fixed_time();
        Thread {
            id: id.to_string(),
            project_id: TEST_PROJECT_ID.to_string(),
            parent_thread_id: None,
            spawn_tool_use_id: None,
            thread_type: "main".to_string(),
            agent_label: None,
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            thinking_effort: None,
            title: None,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            llm_context_version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_agent_task(task_id: &str, thread_id: &str, owner_thread_id: &str) -> AgentTaskInfo {
        let now = fixed_time();
        AgentTaskInfo {
            task_id: task_id.to_string(),
            thread_id: thread_id.to_string(),
            parent_task_id: None,
            owner_thread_id: owner_thread_id.to_string(),
            parent_thread_id: owner_thread_id.to_string(),
            spawn_tool_use_id: format!("tool_{task_id}"),
            agent: "general".to_string(),
            title: "Test agent".to_string(),
            depth: 1,
            execution_mode: AgentTaskExecutionMode::Background,
            status: AgentTaskStatus::Running,
            result: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
            notification_delivered: false,
        }
    }

    fn test_agent_thread(id: &str, parent_thread_id: &str) -> ThreadRecord {
        let now = fixed_time();
        ThreadRecord {
            id: id.to_string(),
            parent_thread_id: Some(parent_thread_id.to_string()),
            spawn_tool_use_id: Some(format!("tool_{id}")),
            thread_type: "agent".to_string(),
            agent_label: Some("general".to_string()),
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            thinking_effort: None,
            title: Some("Test agent".to_string()),
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            llm_context_version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn runtime_child_thread_is_bound_to_server_project_id() {
        let now = fixed_time();
        let runtime = ThreadRecord {
            id: "child".to_string(),
            parent_thread_id: Some("parent".to_string()),
            spawn_tool_use_id: Some("tool".to_string()),
            thread_type: "agent".to_string(),
            agent_label: Some("explorer".to_string()),
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            thinking_effort: None,
            title: Some("Explore".to_string()),
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            llm_context_version: 1,
            created_at: now,
            updated_at: now,
        };

        let stored = thread_from_runtime(TEST_PROJECT_ID, &runtime);

        assert_eq!(stored.project_id, TEST_PROJECT_ID);
        assert_eq!(stored.id, "child");
        assert_eq!(stored.parent_thread_id.as_deref(), Some("parent"));
    }

    #[tokio::test]
    async fn llm_context_loads_only_current_version_and_keeps_old_version() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("t1").unwrap();
        db.create_thread(&test_thread("t1")).await.unwrap();
        let first = Message::from_user_text("old".to_string());
        db.append_llm_message("t1", &first, fixed_time(), &project.thread("t1"))
            .await
            .unwrap();
        let next = vec![
            Message::from_user_text("summary".to_string()),
            first.clone(),
        ];
        assert_eq!(
            db.replace_llm_context("t1", 1, &next, fixed_time(), &project.thread("t1"))
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            db.load_current_llm_messages("t1", &project.thread("t1"))
                .await
                .unwrap(),
            next
        );
        let old_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM llm_messages WHERE thread_id = 't1' AND context_version = 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(old_count, 1);
    }

    #[tokio::test]
    async fn agent_task_creation_is_atomic_and_startup_recovers_terminal_statuses() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("owner").unwrap();
        db.create_thread(&test_thread("owner")).await.unwrap();
        let initial = Message::from_user_text("do work".to_string());

        let task = test_agent_task("task_running", "agent_running", "owner");
        db.create_agent_task(
            TEST_PROJECT_ID,
            &task,
            &test_agent_thread("agent_running", "owner"),
            &initial,
        )
        .await
        .unwrap();
        assert_eq!(db.get_messages("agent_running").await.unwrap().len(), 1);
        assert_eq!(
            db.load_current_llm_messages("agent_running", &project.thread("agent_running"))
                .await
                .unwrap(),
            vec![initial.clone()]
        );

        let cancelling = test_agent_task("task_cancelling", "agent_cancelling", "owner");
        db.create_agent_task(
            TEST_PROJECT_ID,
            &cancelling,
            &test_agent_thread("agent_cancelling", "owner"),
            &initial,
        )
        .await
        .unwrap();
        db.set_agent_tasks_cancelling(&["task_cancelling".to_string()], fixed_time())
            .await
            .unwrap();

        let invalid = test_agent_task("task_invalid", "agent_rolled_back", "missing_owner");
        assert!(
            db.create_agent_task(
                TEST_PROJECT_ID,
                &invalid,
                &test_agent_thread("agent_rolled_back", "owner"),
                &initial,
            )
            .await
            .is_err()
        );
        assert!(db.get_thread("agent_rolled_back").await.unwrap().is_none());

        db.initialize().await.unwrap();
        let tasks = db.list_agent_tasks("owner").await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks
                .iter()
                .find(|task| task.task_id == "task_running")
                .unwrap()
                .status,
            AgentTaskStatus::Interrupted
        );
        assert_eq!(
            tasks
                .iter()
                .find(|task| task.task_id == "task_cancelling")
                .unwrap()
                .status,
            AgentTaskStatus::Cancelled
        );
        assert!(tasks.iter().all(|task| task.completed_at.is_some()));
    }

    #[tokio::test]
    async fn agent_task_notification_is_inserted_exactly_once() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("owner").unwrap();
        db.create_thread(&test_thread("owner")).await.unwrap();
        let task = test_agent_task("task_done", "agent_done", "owner");
        db.create_agent_task(
            TEST_PROJECT_ID,
            &task,
            &test_agent_thread("agent_done", "owner"),
            &Message::from_user_text("do work".to_string()),
        )
        .await
        .unwrap();
        let completed_at = fixed_time();
        db.finish_agent_task(
            "task_done",
            AgentTaskStatus::Completed,
            &AgentTaskResult {
                output: Some("done".to_string()),
                error: None,
                warnings: Vec::new(),
            },
            completed_at,
        )
        .await
        .unwrap();
        let before = Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text("before notification".to_string())],
        );
        db.append_llm_message("owner", &before, fixed_time(), &project.thread("owner"))
            .await
            .unwrap();
        let notification = omini_domain::display::AgentTaskNotification {
            tasks: vec![omini_domain::display::AgentTaskNotificationItem {
                task_id: "task_done".to_string(),
                agent: "general".to_string(),
                title: "Test agent".to_string(),
                status: AgentTaskStatus::Completed,
            }],
            created_at: completed_at,
        };
        let llm_message = Message::from_user_text("agent task completed".to_string());
        for _ in 0..2 {
            db.insert_agent_task_notification(
                "owner",
                &notification,
                &llm_message,
                &["task_done".to_string()],
                completed_at,
            )
            .await
            .unwrap();
        }
        let after = Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text("after notification".to_string())],
        );
        db.append_llm_message("owner", &after, fixed_time(), &project.thread("owner"))
            .await
            .unwrap();

        let ui_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE thread_id = 'owner' AND kind = 'agent_task_notification'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let llm_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM llm_messages WHERE thread_id = 'owner' AND role = 'user'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(ui_count, 1);
        assert_eq!(llm_count, 1);
        assert_eq!(
            db.load_current_llm_messages("owner", &project.thread("owner"))
                .await
                .unwrap(),
            vec![before, llm_message, after]
        );
        assert!(db.list_agent_tasks("owner").await.unwrap()[0].notification_delivered);
        assert!(matches!(
            crate::history::load_messages(&db, "owner", &project.thread("owner"))
                .await
                .as_slice(),
            [omini_domain::display::HistoryItem::AgentTaskNotification(restored)]
                if restored == &notification
        ));
    }

    #[tokio::test]
    async fn agent_compact_advances_llm_version_without_rewriting_ui_history() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("owner").unwrap();
        db.create_thread(&test_thread("owner")).await.unwrap();
        let initial = Message::from_user_text("do work".to_string());
        let task = test_agent_task("task_compact", "agent_compact", "owner");
        db.create_agent_task(
            TEST_PROJECT_ID,
            &task,
            &test_agent_thread("agent_compact", "owner"),
            &initial,
        )
        .await
        .unwrap();
        let compacted = vec![
            Message::from_user_text("summary".to_string()),
            Message::from_user_text("retained tail".to_string()),
        ];

        let next_version = db
            .replace_llm_context(
                "agent_compact",
                1,
                &compacted,
                fixed_time(),
                &project.thread("agent_compact"),
            )
            .await
            .unwrap();

        assert_eq!(next_version, 2);
        assert_eq!(
            db.load_current_llm_messages("agent_compact", &project.thread("agent_compact"))
                .await
                .unwrap(),
            compacted
        );
        let old_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM llm_messages
                WHERE thread_id = 'agent_compact' AND context_version = 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(old_count, 1);
        assert!(matches!(
            crate::history::load_messages(&db, "agent_compact", &project.thread("agent_compact"))
                .await
                .as_slice(),
            [omini_domain::display::HistoryItem::Message(message)] if message == &initial
        ));
    }

    #[tokio::test]
    async fn owner_agent_usage_updates_totals_without_replacing_main_context_usage() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("owner").unwrap();
        db.create_thread(&test_thread("owner")).await.unwrap();
        let main_usage = Usage {
            prompt_tokens: 8,
            completion_tokens: 2,
            cached_tokens: 1,
        };
        db.record_thread_usage("owner", main_usage).await.unwrap();
        let agent_usage = Usage {
            prompt_tokens: 4,
            completion_tokens: 1,
            cached_tokens: 2,
        };

        db.apply_persistence_event(
            &RuntimePersistenceEvent::RecordOwnerAgentUsage {
                thread_id: "owner".to_string(),
                usage: agent_usage,
            },
            TEST_PROJECT_ID,
            &project,
        )
        .await
        .unwrap();

        let owner = db.get_thread("owner").await.unwrap().unwrap();
        assert_eq!(owner.current_context_tokens, 10);
        assert_eq!(owner.total_tokens, 15);
        assert_eq!(owner.total_cached_tokens, 3);
    }

    #[tokio::test]
    async fn images_are_raw_assets_and_rehydrate_to_equivalent_blocks() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("t1").unwrap();
        db.create_thread(&test_thread("t1")).await.unwrap();
        let raw = b"not-really-a-png";
        let image = ContentBlock::Image(omini_domain::message::ImageBlock {
            source: ImageSource {
                source_type: ImageSourceType::Base64,
                media_type: "image/png".to_string(),
                data: BASE64_STANDARD.encode(raw),
            },
        });
        let message = Message::new(Role::User, vec![image.clone()]);
        db.append_llm_message("t1", &message, fixed_time(), &project.thread("t1"))
            .await
            .unwrap();
        let loaded = db
            .load_current_llm_messages("t1", &project.thread("t1"))
            .await
            .unwrap();
        assert_eq!(loaded, vec![message]);
        let db_content: String = sqlx::query_scalar("SELECT content FROM llm_messages LIMIT 1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(!db_content.contains(&BASE64_STANDARD.encode(raw)));
        let asset = fs::read_dir(project.thread("t1").assets_dir())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(asset).unwrap(), raw);
    }

    #[tokio::test]
    async fn compact_reuses_existing_media_asset() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("t1").unwrap();
        db.create_thread(&test_thread("t1")).await.unwrap();
        let image_message = Message::new(
            Role::User,
            vec![ContentBlock::Image(omini_domain::message::ImageBlock {
                source: ImageSource {
                    source_type: ImageSourceType::Base64,
                    media_type: "image/png".to_string(),
                    data: BASE64_STANDARD.encode(b"shared-image"),
                },
            })],
        );
        let thread_dir = project.thread("t1");
        db.append_llm_message("t1", &image_message, fixed_time(), &thread_dir)
            .await
            .unwrap();
        db.replace_llm_context(
            "t1",
            1,
            &[
                Message::from_user_text("summary".to_string()),
                image_message,
            ],
            fixed_time(),
            &thread_dir,
        )
        .await
        .unwrap();

        assert_eq!(fs::read_dir(thread_dir.assets_dir()).unwrap().count(), 1);
        let stored: Vec<String> = sqlx::query_scalar(
            "SELECT content FROM llm_messages
             WHERE thread_id = 't1' AND (
                 (context_version = 1 AND ordinal = 0) OR
                 (context_version = 2 AND ordinal = 1)
             ) ORDER BY context_version",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0], stored[1]);
    }

    #[tokio::test]
    async fn context_version_conflict_cleans_unreferenced_sidecars() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("t1").unwrap();
        db.create_thread(&test_thread("t1")).await.unwrap();
        let thread_dir = project.thread("t1");
        let message = Message::from_user_text("x".repeat(CONTENT_SIZE_THRESHOLD + 1));

        let error = db
            .replace_llm_context("t1", 99, &[message], fixed_time(), &thread_dir)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            StoreError::ContextVersionConflict {
                expected: 99,
                actual: 1
            }
        ));
        assert_eq!(fs::read_dir(thread_dir.sidecars_dir()).unwrap().count(), 0);
        assert_eq!(fs::read_dir(thread_dir.staging_dir()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn large_compact_summary_uses_sidecar_and_keeps_model_snapshot() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("t1").unwrap();
        db.create_thread(&test_thread("t1")).await.unwrap();
        let summary = DisplaySummary {
            id: "summary-1".to_string(),
            title: "Compacted".to_string(),
            markdown: "x".repeat(CONTENT_SIZE_THRESHOLD + 1),
            created_at: fixed_time(),
        };
        db.apply_persistence_event(
            &RuntimePersistenceEvent::InsertCompactSummaryMessage {
                thread_id: "t1".to_string(),
                summary: summary.clone(),
                model_ref: "provider/model".to_string(),
            },
            TEST_PROJECT_ID,
            &project,
        )
        .await
        .unwrap();

        let stored = db.get_messages("t1").await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].model_ref.as_deref(), Some("provider/model"));
        assert!(stored[0].content.contains("sidecar_document"));
        assert!(!stored[0].content.contains(&summary.markdown));
        assert_eq!(
            fs::read_dir(project.thread("t1").sidecars_dir())
                .unwrap()
                .count(),
            1
        );
        let loaded = crate::history::load_messages(&db, "t1", &project.thread("t1")).await;
        assert_eq!(
            loaded,
            vec![omini_domain::display::HistoryItem::Summary(summary)]
        );
    }

    #[tokio::test]
    async fn delete_thread_tree_removes_rows_and_private_directories() {
        let (db, project, _root) = temp_db().await;
        project.create_thread("parent").unwrap();
        project.create_thread("child").unwrap();
        db.create_thread(&test_thread("parent")).await.unwrap();
        let mut child = test_thread("child");
        child.parent_thread_id = Some("parent".to_string());
        child.thread_type = "agent".to_string();
        db.create_thread(&child).await.unwrap();
        db.append_llm_message(
            "child",
            &Message::from_user_text("x".repeat(CONTENT_SIZE_THRESHOLD + 1)),
            fixed_time(),
            &project.thread("child"),
        )
        .await
        .unwrap();

        db.delete_thread_tree("parent", &project).await.unwrap();

        assert!(db.get_thread("parent").await.unwrap().is_none());
        assert!(db.get_thread("child").await.unwrap().is_none());
        assert!(!project.thread("parent").path().exists());
        assert!(!project.thread("child").path().exists());
    }

    #[test]
    fn threshold_is_strictly_greater_than_64_kib() {
        let root = TestRoot::new();
        let thread = ThreadDir::from_path(root.path.join("thread"));
        fs::create_dir_all(thread.path()).unwrap();
        let at_limit = ContentBlock::Text(TextBlock {
            text: "x".repeat(CONTENT_SIZE_THRESHOLD),
        });
        let prepared = prepare_blocks(&[at_limit], &thread).unwrap();
        assert_eq!(prepared.values[0]["type"], "text");
        let above_limit = ContentBlock::Text(TextBlock {
            text: "x".repeat(CONTENT_SIZE_THRESHOLD + 1),
        });
        let prepared = prepare_blocks(&[above_limit], &thread).unwrap();
        assert_eq!(prepared.values[0]["type"], "sidecar");
        let inline = prepare_blocks(
            &[ContentBlock::Text(TextBlock {
                text: "small".to_string(),
            })],
            &thread,
        )
        .unwrap();
        assert_eq!(inline.values[0]["type"], "text");
    }
}
