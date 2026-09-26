use super::*;
use omini_domain::task::{TaskInfo, TaskKind};

#[derive(FromRow)]
struct BackgroundTaskRow {
    task_id: String,
    owner_thread_id: String,
    kind: String,
    title: String,
    status: String,
    result_summary: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl Database {
    pub async fn list_background_tasks(
        &self,
        owner_thread_id: &str,
    ) -> Result<Vec<TaskInfo>, StoreError> {
        let rows = sqlx::query_as::<_, BackgroundTaskRow>(
            "SELECT task_id, owner_thread_id, kind, title, status, result_summary,
                    created_at, updated_at, completed_at
             FROM background_task WHERE owner_thread_id = ?
             ORDER BY updated_at DESC, task_id",
        )
        .bind(owner_thread_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let kind = match row.kind.as_str() {
                    "sub_agent" => TaskKind::SubAgent,
                    "bash" => TaskKind::Bash,
                    value => {
                        return Err(StoreError::InvalidData(format!(
                            "unknown background task kind '{value}'"
                        )));
                    }
                };
                let status = match row.status.as_str() {
                    "running" => TaskStatus::Running,
                    "cancelling" => TaskStatus::Cancelling,
                    "completed" => TaskStatus::Completed,
                    "failed" => TaskStatus::Failed,
                    "cancelled" => TaskStatus::Cancelled,
                    "interrupted" => TaskStatus::Interrupted,
                    value => {
                        return Err(StoreError::InvalidData(format!(
                            "unknown background task status '{value}'"
                        )));
                    }
                };
                Ok(TaskInfo {
                    task_id: row.task_id,
                    owner_thread_id: row.owner_thread_id,
                    kind,
                    title: row.title,
                    status,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                    completed_at: row.completed_at,
                    result_summary: row.result_summary,
                })
            })
            .collect()
    }

    pub async fn upsert_task(&self, task: &omini_domain::task::TaskInfo) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO background_task(
                task_id, owner_thread_id, kind, title, status, result_summary,
                created_at, updated_at, completed_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(task_id) DO UPDATE SET
                owner_thread_id = excluded.owner_thread_id,
                kind = excluded.kind,
                title = excluded.title,
                status = excluded.status,
                result_summary = excluded.result_summary,
                updated_at = excluded.updated_at,
                completed_at = excluded.completed_at",
        )
        .bind(&task.task_id)
        .bind(&task.owner_thread_id)
        .bind(task.kind.as_str())
        .bind(&task.title)
        .bind(task.status.as_str())
        .bind(&task.result_summary)
        .bind(task.created_at)
        .bind(task.updated_at)
        .bind(task.completed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
