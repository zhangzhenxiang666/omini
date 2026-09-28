use crate::entity::{
    AgentRun, AgentStep, AgentTask, AgentTaskDelivery, Attachment, BackgroundTask, LlmMessage,
    Message, Project, Thread, ToolUseExecution,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Toasty(#[from] toasty::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("base64 error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid persisted data: {0}")]
    InvalidData(String),
    /// 按键读取预期必然存在的行(如 thread 的上下文版本)时缺失。
    /// 与 InvalidData 分开:InvalidData 会被映射为客户端输入错误,
    /// 内部数据缺失应当归类为服务端错误。
    #[error("missing expected row: {0}")]
    MissingRow(String),
    #[error(
        "persisted sidecar is too large to load ({actual_bytes} bytes; limit {limit_bytes} bytes)"
    )]
    OversizedSidecar { actual_bytes: u64, limit_bytes: u64 },
    #[error("LLM context version conflict: expected {expected}, found {actual}")]
    ContextVersionConflict { expected: i64, actual: i64 },
    #[error("attachment not found: {0}")]
    AttachmentNotFound(String),
    #[error("database file holds an incompatible schema: {0}")]
    IncompatibleSchema(String),
}

/// 模型清单对应的表数量,ensure_schema 用它区分"全新库"与"异常库"。
const EXPECTED_TABLE_COUNT: i64 = 11;

/// 连接句柄:打开数据库、建库、派发可执行连接。
///
/// 刻意不承载任何查询方法——业务代码通过 [`Database::conn`] 取得
/// toasty 句柄后自行组合查询与事务(sea-orm 风格)。
pub struct Database {
    db: toasty::Db,
}

impl Database {
    pub async fn open(path: &std::path::Path) -> Result<Self, StoreError> {
        // 以 PathBuf 构造驱动,绕过 URL 解析对路径字符的假设。
        let driver = toasty_driver_sqlite::Sqlite::open(path);
        let mut db = toasty::Db::builder()
            .models(toasty::models!(
                Project,
                Thread,
                Message,
                LlmMessage,
                AgentRun,
                AgentStep,
                ToolUseExecution,
                AgentTask,
                AgentTaskDelivery,
                BackgroundTask,
                Attachment,
            ))
            // 进程内单连接:SQLite 驱动无法按连接设置 busy_timeout,
            // 多连接并发写会立即返回 SQLITE_BUSY;单连接让语句天然排队,
            // 对本服务(唯一写入进程、短查询)是最稳妥的形态。
            // 兜底等待上限防止悬挂事务拖垮整个持久层。
            //
            // 部署假设:同一数据库文件只有一个 daemon 进程在写。
            // 业务层的读-判-写模式(replace_llm_context、归档、初始标题)
            // 依赖该假设获得原子性;busy_timeout 只兜跨进程读锁,
            // 不承诺跨进程并发写的正确性——外部工具写库属自担风险.
            .pool_wait_timeout(Some(std::time::Duration::from_secs(30)))
            .max_pool_size(1)
            .build(driver)
            .await?;

        // 连接级 PRAGMA 在单连接池的常驻连接上设置一次即可。
        // WAL 是数据库文件的持久属性;不能在事务内执行。
        toasty::sql::query("PRAGMA journal_mode=WAL")
            .exec(&mut db)
            .await?;
        // 跨进程锁竞争(如用户用 sqlite3 CLI 同时写库)时等待而非立即报错。
        toasty::sql::query("PRAGMA busy_timeout=5000")
            .exec(&mut db)
            .await?;

        let database = Self { db };
        database.ensure_schema().await?;
        Ok(database)
    }

    /// 克隆出可执行句柄;toasty 的 Db 是进程内共享的轻量句柄,
    /// 业务代码用它执行查询,或经 `transaction()` 开启事务。
    pub fn conn(&self) -> toasty::Db {
        self.db.clone()
    }

    /// 建库门:push_schema 生成的 DDL 不带 IF NOT EXISTS,不可重复执行。
    /// 以用户表数量区分三种状态——空库建表、完整库跳过、部分表视为
    /// 异常文件,报错引导删除重建,不做猜测性修复。
    async fn ensure_schema(&self) -> Result<(), StoreError> {
        let mut db = self.conn();
        let rows = toasty::sql::query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .exec(&mut db)
        .await?;
        let count = rows
            .first()
            .and_then(|row| row.as_record())
            .and_then(|record| record.as_slice().first())
            .and_then(toasty::stmt::Value::to_i64)
            .ok_or_else(|| StoreError::InvalidData("table count query failed".into()))?;
        match count {
            0 => {
                db.push_schema().await?;
            }
            EXPECTED_TABLE_COUNT => {}
            other => {
                return Err(StoreError::IncompatibleSchema(format!(
                    "expected {EXPECTED_TABLE_COUNT} tables, found {other}; \
                     remove the database file to rebuild"
                )));
            }
        }
        Ok(())
    }
}
