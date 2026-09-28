use jiff::Timestamp;
use omini_domain::task::{TaskKind, TaskStatus};

/// 通用后台任务表:跨类型(sub_agent/bash)的任务列表投影,
/// `notification_delivered` 记录完成通知是否已写入时间线。
#[derive(Debug, Clone, toasty::Model)]
#[table = "background_task"]
#[index(name = "idx_background_task_owner", owner_thread_id, updated_at)]
pub struct BackgroundTask {
    #[key]
    pub task_id: String,
    pub owner_thread_id: String,
    pub kind: TaskKind,
    pub title: String,
    pub status: TaskStatus,
    pub result_summary: Option<String>,
    #[auto]
    pub created_at: Timestamp,
    #[auto]
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub notification_delivered: bool,
}
