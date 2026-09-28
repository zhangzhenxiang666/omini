use super::Store;
use crate::store::StoreError;
use omini_domain::task::TaskInfo;
use omini_entity::BackgroundTask;

impl Store {
    pub async fn list_background_tasks(
        &self,
        owner_thread_id: &str,
    ) -> Result<Vec<TaskInfo>, StoreError> {
        let mut db = self.conn();
        Ok(BackgroundTask::filter_by_owner_thread_id(owner_thread_id)
            .order_by((
                BackgroundTask::fields().updated_at().desc(),
                BackgroundTask::fields().task_id().asc(),
            ))
            .exec(&mut db)
            .await?
            .into_iter()
            .map(task_info_from_row)
            .collect())
    }

    pub async fn upsert_task(&self, task: &TaskInfo) -> Result<(), StoreError> {
        let mut db = self.conn();
        // toasty upsert 的顶层 setter 同时进入插入与冲突更新两个分支,
        // 因此"仅在首次写入时确定"的列必须放 on_create:
        // created_at 保留首次创建时间;notification_delivered 是任务完成通知的
        // 幂等闸门,重放 UpsertTask 事件绝不能把它重置回 false。
        BackgroundTask::upsert_by_task_id(&task.task_id)
            .on_create(|create| {
                create
                    .created_at(task.created_at)
                    .notification_delivered(false)
            })
            .owner_thread_id(task.owner_thread_id.clone())
            .kind(task.kind)
            .title(task.title.clone())
            .status(task.status)
            .result_summary(task.result_summary.clone())
            .updated_at(task.updated_at)
            .completed_at(task.completed_at)
            .exec(&mut db)
            .await?;
        Ok(())
    }
}

/// 后台任务行到领域任务信息的映射(kind/status 是模型上的领域枚举)。
fn task_info_from_row(row: BackgroundTask) -> TaskInfo {
    TaskInfo {
        task_id: row.task_id,
        owner_thread_id: row.owner_thread_id,
        kind: row.kind,
        title: row.title,
        status: row.status,
        created_at: row.created_at,
        updated_at: row.updated_at,
        completed_at: row.completed_at,
        result_summary: row.result_summary,
    }
}
