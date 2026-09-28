//! 持久化业务层:在 omini-entity 的模型之上组合查询、事务与业务策略。
//!
//! entity crate 声明"数据长什么样"并生成访问器;本层用 toasty 查询 API
//! 直接组合它们,并承载业务规则——幂等闸门、投递结算、启动恢复、
//! 线程树删除的依赖序等。[`Store`] 包装 entity 的连接句柄,
//! 打开时先执行启动恢复归一化。

mod agent_deliveries;
mod agent_runs;
mod agent_tasks;
mod attachments;
mod context;
mod history;
mod messages;
mod persistence;
mod projects;
mod recovery;
mod task_store;
mod threads;

pub use history::{load_agent_tasks, load_messages};
pub use messages::NewMessage;
pub use omini_entity::{
    AgentRun, AgentStep, AgentTask, Attachment, BackgroundTask, DeliveryStatus, LlmMessage,
    Message, MessageKind, Project, StoreError, Thread, ToolUseExecution, load_asset,
    persist_staged_asset, stored_asset_path, thread_from_runtime,
};

use std::path::Path;

/// 业务层句柄:包装 entity 连接,负责打开与启动恢复。
pub struct Store {
    db: omini_entity::Database,
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let db = omini_entity::Database::open(path).await?;
        let store = Self { db };
        // 上次进程退出时未结束的任务与 Run 在此收敛为终态。
        store.recover_interrupted_state().await?;
        Ok(store)
    }

    /// toasty 可执行句柄:业务查询、事务与集成测试的类型化断言从这里取得。
    pub fn conn(&self) -> toasty::Db {
        self.db.conn()
    }
}
