//! SQLite 持久化层。
//!
//! server 在这里保存会话元数据、消息历史和运行时发来的持久化事件。大型
//! `ContentBlock` 会拆到 sidecar 文件，避免单行 JSON 过大影响数据库读写。

use chrono::{DateTime, Utc};
use omini_domain::display::{DisplayMessage, DisplayPlan, DisplaySummary};
use omini_domain::message::ContentBlock;
use omini_domain::usage::Usage;
use omini_runtime_api::persistence::{RuntimePersistenceEvent, SessionRecord};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{FromRow, SqlitePool};
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

/// server 持久化层统一错误。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// server 使用 core persistence 的会话记录作为数据库会话模型。
pub(crate) type Session = SessionRecord;

/// 从数据库读出的消息行。
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub kind: String,
    pub created_at: DateTime<Utc>,
}

/// 准备写入数据库的新消息。
pub struct NewMessage {
    pub session_id: String,
    pub role: String,
    pub blocks: Vec<ContentBlock>,
    pub kind: String,
    pub created_at: DateTime<Utc>,
    pub blocks_dir: PathBuf,
}

/// `sessions` 表的原始行结构。
#[derive(Debug, Clone, FromRow)]
struct SessionRow {
    id: String,
    project_path: String,
    parent_session_id: Option<String>,
    spawn_tool_use_id: Option<String>,
    session_type: String,
    agent_label: Option<String>,
    provider: String,
    model: String,
    thinking_effort: Option<String>,
    title: Option<String>,
    current_context_tokens: i64,
    total_tokens: i64,
    total_cached_tokens: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// `messages` 表的原始行结构。
#[derive(Debug, Clone, FromRow)]
struct StoredMessageRow {
    id: i64,
    session_id: String,
    role: String,
    content: String,
    kind: String,
    created_at: DateTime<Utc>,
}

impl From<SessionRow> for Session {
    fn from(row: SessionRow) -> Self {
        Self {
            id: row.id,
            project_path: row.project_path,
            parent_session_id: row.parent_session_id,
            spawn_tool_use_id: row.spawn_tool_use_id,
            session_type: row.session_type,
            agent_label: row.agent_label,
            provider: row.provider,
            model: row.model,
            thinking_effort: row.thinking_effort,
            title: row.title,
            current_context_tokens: row.current_context_tokens,
            total_tokens: row.total_tokens,
            total_cached_tokens: row.total_cached_tokens,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<StoredMessageRow> for StoredMessage {
    fn from(row: StoredMessageRow) -> Self {
        Self {
            id: row.id,
            session_id: row.session_id,
            role: row.role,
            content: row.content,
            kind: row.kind,
            created_at: row.created_at,
        }
    }
}

/// SQLite 数据库句柄和所有持久化操作入口。
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// 打开（或创建）SQLite 数据库并初始化表结构。
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await?;

        let db = Self { pool };
        db.initialize().await?;
        Ok(db)
    }

    /// 初始化表结构，并对旧数据库补齐缺失列。
    async fn initialize(&self) -> Result<(), StoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sessions (
                id                TEXT PRIMARY KEY,
                project_path      TEXT NOT NULL,
                parent_session_id TEXT REFERENCES sessions(id),
                spawn_tool_use_id TEXT,
                session_type      TEXT NOT NULL DEFAULT 'main',
                agent_label       TEXT,
                provider          TEXT NOT NULL DEFAULT '',
                model             TEXT NOT NULL DEFAULT '',
                thinking_effort   TEXT,
                title             TEXT,
                current_context_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens      INTEGER NOT NULL DEFAULT 0,
                total_cached_tokens INTEGER NOT NULL DEFAULT 0,
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        self.ensure_sessions_column("spawn_tool_use_id", "TEXT")
            .await?;
        self.ensure_sessions_column("current_context_tokens", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.ensure_sessions_column("total_tokens", "INTEGER NOT NULL DEFAULT 0")
            .await?;
        self.ensure_sessions_column("total_cached_tokens", "INTEGER NOT NULL DEFAULT 0")
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role            TEXT NOT NULL,
                content         TEXT NOT NULL,
                kind            TEXT NOT NULL DEFAULT 'normal',
                created_at      TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        // New index on (session_id, id) for efficient session message queries
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id, id)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_sessions_project
            ON sessions(project_path)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_sessions_parent
            ON sessions(parent_session_id)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Session CRUD
    // -----------------------------------------------------------------------

    /// 插入一条新的会话记录。
    pub async fn create_session(&self, session: &Session) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO sessions
            (id, project_path, parent_session_id, spawn_tool_use_id, session_type, agent_label,
            provider, model, thinking_effort, title, current_context_tokens, total_tokens,
            total_cached_tokens, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&session.id)
        .bind(&session.project_path)
        .bind(&session.parent_session_id)
        .bind(&session.spawn_tool_use_id)
        .bind(&session.session_type)
        .bind(&session.agent_label)
        .bind(&session.provider)
        .bind(&session.model)
        .bind(&session.thinking_effort)
        .bind(&session.title)
        .bind(session.current_context_tokens)
        .bind(session.total_tokens)
        .bind(session.total_cached_tokens)
        .bind(session.created_at)
        .bind(session.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 按 ID 读取会话元数据。
    pub async fn get_session(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let row = sqlx::query_as::<_, SessionRow>("SELECT * FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(Into::into))
    }

    /// 列出项目下的主会话，按最近更新时间倒序返回。
    pub async fn list_sessions(&self, project_path: &str) -> Result<Vec<Session>, StoreError> {
        let rows = sqlx::query_as::<_, SessionRow>(
            "SELECT * FROM sessions WHERE project_path = ? AND session_type = 'main' ORDER BY updated_at DESC, created_at DESC",
        )
        .bind(project_path)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// 列出某个主会话派生出的子 agent 会话。
    pub async fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, StoreError> {
        let rows = sqlx::query_as::<_, SessionRow>(
            "SELECT * FROM sessions WHERE parent_session_id = ? ORDER BY created_at ASC",
        )
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// 记录主会话 usage，并同步当前 context token 计数。
    pub async fn record_session_usage(&self, id: &str, usage: Usage) -> Result<(), StoreError> {
        let now = Utc::now();
        let total_tokens = usage_tokens_i64(usage);
        let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
        sqlx::query(
            "UPDATE sessions
            SET current_context_tokens = ?,
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

    pub async fn record_parent_subagent_usage(
        &self,
        id: &str,
        usage: Usage,
    ) -> Result<(), StoreError> {
        self.record_session_total_usage(id, usage).await
    }

    /// 只累计 total usage，不覆盖当前 context token。
    pub async fn record_session_total_usage(
        &self,
        id: &str,
        usage: Usage,
    ) -> Result<(), StoreError> {
        let now = Utc::now();
        let total_tokens = usage_tokens_i64(usage);
        let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
        sqlx::query(
            "UPDATE sessions
            SET total_tokens = total_tokens + ?,
                total_cached_tokens = total_cached_tokens + ?,
                updated_at = ?
            WHERE id = ?",
        )
        .bind(total_tokens)
        .bind(cached_tokens)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 更新会话的 updated_at 时间戳（在发起 query 时调用）。
    pub async fn update_session_updated_at(&self, id: &str) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更新会话的提供商 / 模型 / 思考程度配置。
    pub async fn update_session_config(
        &self,
        id: &str,
        provider: &str,
        model: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE sessions SET provider = ?, model = ?, thinking_effort = ?, updated_at = ? WHERE id = ?",
        )
        .bind(provider)
        .bind(model)
        .bind(thinking_effort)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 更新会话的思考程度配置。
    pub async fn update_session_thinking_effort(
        &self,
        id: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query("UPDATE sessions SET thinking_effort = ?, updated_at = ? WHERE id = ?")
            .bind(thinking_effort)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更新会话标题。
    pub async fn update_session_title(&self, id: &str, title: &str) -> Result<(), StoreError> {
        let now = Utc::now();
        sqlx::query("UPDATE sessions SET title = ?, updated_at = ? WHERE id = ?")
            .bind(title)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 仅当会话仍为空白且没有消息时设置初始标题。
    pub async fn set_initial_session_title(
        &self,
        id: &str,
        title: &str,
    ) -> Result<bool, StoreError> {
        let now = Utc::now();
        // 首条用户输入只能在会话仍无标题且无消息时设为初始标题，避免覆盖用户重命名。
        let result = sqlx::query(
            "UPDATE sessions
            SET title = ?, updated_at = ?
            WHERE id = ?
                AND (title IS NULL OR TRIM(title) = '')
                AND NOT EXISTS (
                    SELECT 1 FROM messages WHERE session_id = ? LIMIT 1
                )",
        )
        .bind(title)
        .bind(now)
        .bind(id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    // -----------------------------------------------------------------------
    // Message CRUD
    // -----------------------------------------------------------------------

    /// 写入普通 LLM 消息，必要时把大内容块拆到 sidecar。
    pub async fn insert_message(&self, msg: &NewMessage) -> Result<(), StoreError> {
        let blocks_json = {
            let stored = prepare_blocks(&msg.blocks, &msg.blocks_dir)?;
            serde_json::to_string(&stored)?
        };

        sqlx::query(
            "INSERT INTO messages (session_id, role, content, kind, created_at)
            VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&msg.session_id)
        .bind(&msg.role)
        .bind(&blocks_json)
        .bind(&msg.kind)
        .bind(msg.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 写入只用于 UI/SQLite 展示的消息。
    pub async fn insert_display_message(
        &self,
        session_id: &str,
        display: &DisplayMessage,
    ) -> Result<(), StoreError> {
        self.insert_display_message_with_created_at(session_id, display, Utc::now())
            .await
    }

    async fn insert_display_message_with_created_at(
        &self,
        session_id: &str,
        display: &DisplayMessage,
        created_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let content = serde_json::to_string(display)?;
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, kind, created_at)
            VALUES (?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(display.role.to_string())
        .bind(content)
        .bind("display")
        .bind(created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 写入独立 plan 记录，避免从 assistant 文本里二次解析。
    pub async fn insert_plan_message(
        &self,
        session_id: &str,
        plan: &DisplayPlan,
    ) -> Result<(), StoreError> {
        let content = serde_json::to_string(plan)?;
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, kind, created_at)
            VALUES (?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind("assistant")
        .bind(content)
        .bind("plan")
        .bind(plan.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 写入压缩摘要展示记录。
    pub async fn insert_compact_summary_message(
        &self,
        session_id: &str,
        summary: &DisplaySummary,
    ) -> Result<(), StoreError> {
        let content = serde_json::to_string(summary)?;
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, kind, created_at)
            VALUES (?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind("assistant")
        .bind(content)
        .bind("compact_summary")
        .bind(summary.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 按写入顺序读取一个会话的全部消息行。
    pub async fn get_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>, StoreError> {
        let rows = sqlx::query_as::<_, StoredMessageRow>(
            "SELECT * FROM messages WHERE session_id = ? ORDER BY id",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// 获取指定会话第一条消息的纯文本内容。
    pub async fn get_first_message_text(&self, session_id: &str) -> Result<String, StoreError> {
        let row = sqlx::query_as::<_, StoredMessageRow>(
            "SELECT * FROM messages WHERE session_id = ? ORDER BY id ASC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(msg) => Ok(extract_message_text(&msg.content)),
            None => Ok(String::new()),
        }
    }

    /// 消费 core 发出的持久化事件并映射到具体数据库操作。
    pub(crate) async fn apply_persistence_event(
        &self,
        event: RuntimePersistenceEvent,
    ) -> Result<(), StoreError> {
        // server 是 core 持久化事件到 SQLite 的边界；这里保持事件到表操作的一一映射。
        match event {
            RuntimePersistenceEvent::CreateSession(session) => self.create_session(&session).await,
            RuntimePersistenceEvent::UpdateSessionUpdatedAt { session_id } => {
                self.update_session_updated_at(&session_id).await
            }
            RuntimePersistenceEvent::UpdateSessionConfig {
                session_id,
                provider,
                model,
                thinking_effort,
            } => {
                self.update_session_config(
                    &session_id,
                    &provider,
                    &model,
                    thinking_effort.as_deref(),
                )
                .await
            }
            RuntimePersistenceEvent::UpdateSessionThinkingEffort {
                session_id,
                thinking_effort,
            } => {
                self.update_session_thinking_effort(&session_id, thinking_effort.as_deref())
                    .await
            }
            RuntimePersistenceEvent::InsertMessage {
                session_id,
                role,
                blocks,
                kind,
                created_at,
                blocks_dir,
            } => {
                self.insert_message(&NewMessage {
                    session_id,
                    role,
                    blocks,
                    kind,
                    created_at,
                    blocks_dir,
                })
                .await
            }
            RuntimePersistenceEvent::InsertDisplayMessage {
                session_id,
                display,
                created_at,
            } => {
                self.insert_display_message_with_created_at(&session_id, &display, created_at)
                    .await
            }
            RuntimePersistenceEvent::InsertPlanMessage { session_id, plan } => {
                self.insert_plan_message(&session_id, &plan).await
            }
            RuntimePersistenceEvent::InsertCompactSummaryMessage {
                session_id,
                summary,
            } => {
                self.insert_compact_summary_message(&session_id, &summary)
                    .await
            }
            RuntimePersistenceEvent::RecordSessionUsage { session_id, usage } => {
                self.record_session_usage(&session_id, usage).await
            }
            RuntimePersistenceEvent::RecordSessionTotalUsage { session_id, usage } => {
                self.record_session_total_usage(&session_id, usage).await
            }
            RuntimePersistenceEvent::RecordParentSubagentUsage { session_id, usage } => {
                self.record_parent_subagent_usage(&session_id, usage).await
            }
        }
    }

    /// 旧数据库轻量迁移：缺列时追加新列。
    async fn ensure_sessions_column(
        &self,
        column: &str,
        definition: &str,
    ) -> Result<(), StoreError> {
        let rows = sqlx::query("PRAGMA table_info(sessions)")
            .fetch_all(&self.pool)
            .await?;
        let exists = rows.iter().any(|row| {
            use sqlx::Row;
            row.try_get::<String, _>("name")
                .map(|name| name == column)
                .unwrap_or(false)
        });
        if !exists {
            let sql = format!("ALTER TABLE sessions ADD COLUMN {column} {definition}");
            sqlx::query(&sql).execute(&self.pool).await?;
        }
        Ok(())
    }
}

fn usage_tokens_i64(usage: Usage) -> i64 {
    usage_usize_to_i64(usage.total_tokens())
}

fn usage_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// 单个 `ContentBlock` 超过该大小后会存入 sidecar 文件。
const BLOCK_SIZE_THRESHOLD: usize = 10 * 1024;

/// 将内容块转换成可写入 messages.content 的 JSON，必要时生成 sidecar 引用。
pub(crate) fn prepare_blocks(
    blocks: &[ContentBlock],
    blocks_dir: &Path,
) -> Result<Vec<serde_json::Value>, StoreError> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        let value = serde_json::to_value(block)?;
        let json_str = serde_json::to_string(block)?;

        if json_str.len() > BLOCK_SIZE_THRESHOLD {
            // 大块内容落 sidecar，messages.content 只保留轻量索引字段，避免 SQLite 行膨胀。
            let file_id = Uuid::new_v4().to_string();
            let block_dir = blocks_dir.join(&file_id);
            std::fs::create_dir_all(&block_dir)?;
            std::fs::write(block_dir.join("block.json"), &json_str)?;

            let mut ref_val = serde_json::Map::new();
            if let Some(obj) = value.as_object() {
                // type/id 等字段保留在行内，列表页和调试时不必读取 sidecar 才能识别块。
                for key in ["type", "id", "name", "tool_use_id", "is_error"] {
                    if let Some(v) = obj.get(key) {
                        ref_val.insert(key.to_string(), v.clone());
                    }
                }
            }
            ref_val.insert("file".to_string(), serde_json::json!(file_id));
            out.push(serde_json::Value::Object(ref_val));
        } else {
            out.push(value);
        }
    }
    Ok(out)
}

/// 从行内 JSON 或 sidecar 文件恢复完整内容块。
pub(crate) fn load_blocks(
    stored: &[serde_json::Value],
    blocks_dir: &Path,
) -> Result<Vec<ContentBlock>, StoreError> {
    let mut blocks = Vec::with_capacity(stored.len());
    for val in stored {
        // 带 file 字段的是 sidecar 引用；普通块仍按行内 JSON 解析，兼容小消息和旧数据。
        let block = if let Some(file_id) = val.get("file").and_then(|v| v.as_str()) {
            let file_path = blocks_dir.join(file_id).join("block.json");
            let content = std::fs::read_to_string(&file_path)?;
            serde_json::from_str::<ContentBlock>(&content)?
        } else {
            serde_json::from_value(val.clone())?
        };
        blocks.push(block);
    }
    Ok(blocks)
}

/// 从不同消息 JSON 形状中提取可作为标题候选的纯文本。
fn extract_message_text(content_json: &str) -> String {
    if let Ok(display) = serde_json::from_str::<DisplayMessage>(content_json) {
        return display.text.replace('\n', " ").replace('\r', "");
    }
    if let Ok(summary) = serde_json::from_str::<DisplaySummary>(content_json) {
        return summary.markdown.replace('\n', " ").replace('\r', "");
    }

    if let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(content_json) {
        let texts: Vec<String> = values
            .iter()
            .filter_map(|v| {
                if v.get("type").and_then(|t| t.as_str()) == Some("text") {
                    v.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.replace('\n', " ").replace('\r', ""))
                } else {
                    None
                }
            })
            .collect();
        return texts.join(" ");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    async fn temp_db() -> Database {
        let path = std::env::temp_dir().join(format!("omini-db-test-{}.sqlite", Uuid::new_v4()));
        Database::open(&path).await.expect("db should open")
    }

    fn test_session(id: &str) -> Session {
        let now = Utc::now();
        Session {
            id: id.to_string(),
            project_path: "/tmp/project".to_string(),
            parent_session_id: None,
            spawn_tool_use_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            thinking_effort: None,
            title: None,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn session_usage_fields_default_to_zero() {
        let db = temp_db().await;
        db.create_session(&test_session("s1"))
            .await
            .expect("session should insert");

        let session = db
            .get_session("s1")
            .await
            .expect("session should load")
            .expect("session should exist");

        assert_eq!(session.current_context_tokens, 0);
        assert_eq!(session.total_tokens, 0);
        assert_eq!(session.total_cached_tokens, 0);
    }

    #[tokio::test]
    async fn record_session_usage_updates_current_and_totals() {
        let db = temp_db().await;
        db.create_session(&test_session("s1"))
            .await
            .expect("session should insert");

        db.record_session_usage(
            "s1",
            Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                cached_tokens: 3,
            },
        )
        .await
        .expect("usage should update");
        db.record_session_usage(
            "s1",
            Usage {
                prompt_tokens: 2,
                completion_tokens: 4,
                cached_tokens: 1,
            },
        )
        .await
        .expect("usage should update");

        let session = db
            .get_session("s1")
            .await
            .expect("session should load")
            .expect("session should exist");

        assert_eq!(session.current_context_tokens, 6);
        assert_eq!(session.total_tokens, 21);
        assert_eq!(session.total_cached_tokens, 4);
    }

    #[tokio::test]
    async fn record_parent_subagent_usage_only_updates_totals() {
        let db = temp_db().await;
        db.create_session(&test_session("s1"))
            .await
            .expect("session should insert");
        db.record_session_usage(
            "s1",
            Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                cached_tokens: 3,
            },
        )
        .await
        .expect("usage should update");

        db.record_parent_subagent_usage(
            "s1",
            Usage {
                prompt_tokens: 7,
                completion_tokens: 8,
                cached_tokens: 4,
            },
        )
        .await
        .expect("subagent usage should update");

        let session = db
            .get_session("s1")
            .await
            .expect("session should load")
            .expect("session should exist");

        assert_eq!(session.current_context_tokens, 15);
        assert_eq!(session.total_tokens, 30);
        assert_eq!(session.total_cached_tokens, 7);
    }

    #[tokio::test]
    async fn record_session_total_usage_only_updates_totals() {
        let db = temp_db().await;
        db.create_session(&test_session("s1"))
            .await
            .expect("session should insert");
        db.record_session_usage(
            "s1",
            Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                cached_tokens: 3,
            },
        )
        .await
        .expect("usage should update");

        db.record_session_total_usage(
            "s1",
            Usage {
                prompt_tokens: 7,
                completion_tokens: 8,
                cached_tokens: 4,
            },
        )
        .await
        .expect("total usage should update");

        let session = db
            .get_session("s1")
            .await
            .expect("session should load")
            .expect("session should exist");

        assert_eq!(session.current_context_tokens, 15);
        assert_eq!(session.total_tokens, 30);
        assert_eq!(session.total_cached_tokens, 7);
    }

    #[tokio::test]
    async fn set_initial_session_title_updates_only_untitled_empty_sessions() {
        let db = temp_db().await;
        db.create_session(&test_session("s1"))
            .await
            .expect("session should insert");

        assert!(
            db.set_initial_session_title("s1", "first message")
                .await
                .expect("title should update")
        );
        assert_eq!(
            db.get_session("s1")
                .await
                .expect("session should load")
                .expect("session should exist")
                .title
                .as_deref(),
            Some("first message")
        );

        assert!(
            !db.set_initial_session_title("s1", "replacement")
                .await
                .expect("title should not update")
        );
        assert_eq!(
            db.get_session("s1")
                .await
                .expect("session should load")
                .expect("session should exist")
                .title
                .as_deref(),
            Some("first message")
        );
    }

    #[tokio::test]
    async fn set_initial_session_title_skips_sessions_with_messages() {
        let db = temp_db().await;
        db.create_session(&test_session("s1"))
            .await
            .expect("session should insert");
        db.insert_display_message(
            "s1",
            &DisplayMessage {
                role: omini_domain::message::Role::User,
                text: "first message".to_string(),
                mentions: Vec::new(),
            },
        )
        .await
        .expect("message should insert");

        assert!(
            !db.set_initial_session_title("s1", "late title")
                .await
                .expect("title should not update")
        );
        assert_eq!(
            db.get_session("s1")
                .await
                .expect("session should load")
                .expect("session should exist")
                .title,
            None
        );
    }
}
