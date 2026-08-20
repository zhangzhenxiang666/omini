use super::*;

impl Database {
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
    pub async fn finish_agent_task(
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

    pub async fn set_agent_tasks_cancelling(
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

    pub async fn insert_agent_task_notification(
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
}
