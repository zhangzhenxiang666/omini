use crate::types::display::{DisplayMessage, DisplayPlan, DisplaySummary};
use crate::types::message::ContentBlock;
use crate::types::usage::Usage;
use chrono::{DateTime, Utc};
use serde_json;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{FromRow, SqlitePool};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// 单个 ContentBlock 序列化后的字节数超过此阈值时，溢出到独立文件。
const BLOCK_SIZE_THRESHOLD: usize = 10 * 1024; // 10 KB

/// 将 ContentBlock 准备为可存储的 JSON Value 数组。
///
/// 小于阈值的 block 以内联 JSON 存储；超过阈值的 block 写入
/// `blocks_dir/<uuid>/block.json`，在 JSON 中以轻量引用替换。
pub fn prepare_blocks(
    blocks: &[ContentBlock],
    blocks_dir: &Path,
) -> Result<Vec<serde_json::Value>, DbError> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        let value = serde_json::to_value(block)?;
        let json_str = serde_json::to_string(block)?;

        if json_str.len() > BLOCK_SIZE_THRESHOLD {
            let file_id = Uuid::new_v4().to_string();
            let block_dir = blocks_dir.join(&file_id);
            fs::create_dir_all(&block_dir)?;
            fs::write(block_dir.join("block.json"), &json_str)?;

            // 轻量引用：保留 type 和 UI 渲染需要的元字段
            let mut ref_val = serde_json::Map::new();
            if let Some(obj) = value.as_object() {
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

/// 从存储的 JSON Value 数组还原为完整的 `Vec<ContentBlock>`。
///
/// 遇到 `file` 引用时，从 `blocks_dir/<file_id>/block.json` 加载完整内容。
pub fn load_blocks(
    stored: &[serde_json::Value],
    blocks_dir: &Path,
) -> Result<Vec<ContentBlock>, DbError> {
    let mut blocks = Vec::with_capacity(stored.len());
    for val in stored {
        let block = if let Some(file_id) = val.get("file").and_then(|v| v.as_str()) {
            let file_path = blocks_dir.join(file_id).join("block.json");
            let content = fs::read_to_string(&file_path)?;
            serde_json::from_str::<ContentBlock>(&content)?
        } else {
            serde_json::from_value(val.clone())?
        };
        blocks.push(block);
    }
    Ok(blocks)
}

#[derive(Debug, Clone, FromRow)]
pub struct Session {
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

#[derive(Debug, Clone, FromRow)]
pub struct StoredMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub kind: String,
    pub created_at: DateTime<Utc>,
}

pub struct NewMessage {
    pub session_id: String,
    pub role: String,
    /// ContentBlock 数组，在 `insert_message` 内部序列化为 JSON 并按需溢出到文件
    pub blocks: Vec<ContentBlock>,
    pub kind: String,
    pub created_at: DateTime<Utc>,
    /// 用于存储大块的目录（`sessions/<id>/blocks/`），由调用方提供
    pub blocks_dir: std::path::PathBuf,
}

pub struct Database {
    pool: SqlitePool,
}

/// 全局单例，程序启动时由 `init_global` 初始化一次。
static GLOBAL_DB: OnceLock<Database> = OnceLock::new();

/// 初始化全局数据库实例。应在 `main.rs` 中 `open` 后调用一次。
///
/// # Panics
/// 重复调用会 panic。
pub fn init_global(db: Database) {
    GLOBAL_DB
        .set(db)
        .unwrap_or_else(|_| panic!("global database already initialized"));
}

/// 返回全局数据库引用。
///
/// # Panics
/// 未调用 `init_global` 直接调用此函数会 panic。
pub fn global_db() -> &'static Database {
    GLOBAL_DB
        .get()
        .expect("database not initialized; call init_global first")
}

impl Database {
    /// 打开（或创建）SQLite 数据库并初始化表结构。
    pub async fn open(path: &Path) -> Result<Self, DbError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await?;

        let db = Self { pool };
        db.initialize().await?;
        Ok(db)
    }

    async fn initialize(&self) -> Result<(), DbError> {
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

    pub async fn create_session(&self, session: &Session) -> Result<(), DbError> {
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

    pub async fn get_session(&self, id: &str) -> Result<Option<Session>, DbError> {
        let row = sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    pub async fn list_sessions(&self, project_path: &str) -> Result<Vec<Session>, DbError> {
        let rows = sqlx::query_as::<_, Session>(
            "SELECT * FROM sessions WHERE project_path = ? AND session_type = 'main' ORDER BY updated_at DESC, created_at DESC",
        )
        .bind(project_path)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, DbError> {
        let rows = sqlx::query_as::<_, Session>(
            "SELECT * FROM sessions WHERE parent_session_id = ? ORDER BY created_at ASC",
        )
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn record_session_usage(&self, id: &str, usage: Usage) -> Result<(), DbError> {
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
    ) -> Result<(), DbError> {
        self.record_session_total_usage(id, usage).await
    }

    pub async fn record_session_total_usage(&self, id: &str, usage: Usage) -> Result<(), DbError> {
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
    pub async fn update_session_updated_at(&self, id: &str) -> Result<(), DbError> {
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
    ) -> Result<(), DbError> {
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
    ) -> Result<(), DbError> {
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
    pub async fn update_session_title(&self, id: &str, title: &str) -> Result<(), DbError> {
        let now = Utc::now();
        sqlx::query("UPDATE sessions SET title = ?, updated_at = ? WHERE id = ?")
            .bind(title)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Message CRUD
    // -----------------------------------------------------------------------

    pub async fn insert_message(&self, msg: &NewMessage) -> Result<(), DbError> {
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

    pub async fn insert_display_message(
        &self,
        session_id: &str,
        display: &DisplayMessage,
    ) -> Result<(), DbError> {
        let content = serde_json::to_string(display)?;
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, kind, created_at)
            VALUES (?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(display.role.to_string())
        .bind(content)
        .bind("display")
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_plan_message(
        &self,
        session_id: &str,
        plan: &DisplayPlan,
    ) -> Result<(), DbError> {
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

    pub async fn insert_compact_summary_message(
        &self,
        session_id: &str,
        summary: &DisplaySummary,
    ) -> Result<(), DbError> {
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

    pub async fn get_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>, DbError> {
        let rows = sqlx::query_as::<_, StoredMessage>(
            "SELECT * FROM messages WHERE session_id = ? ORDER BY id",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// 获取指定会话第一条消息的纯文本内容。
    pub async fn get_first_message_text(&self, session_id: &str) -> Result<String, DbError> {
        let row = sqlx::query_as::<_, StoredMessage>(
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

    async fn ensure_sessions_column(&self, column: &str, definition: &str) -> Result<(), DbError> {
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

/// 从存储的消息 content JSON（ContentBlock 数组）中提取纯文本。
/// 跳过 file 引用的大块、tool_use、thinking 等非文本块。
pub fn extract_message_text(content_json: &str) -> String {
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
}
