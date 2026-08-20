use super::*;

impl Database {
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
}

pub fn thread_from_runtime(project_id: &str, thread: &ThreadRecord) -> Thread {
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
fn usage_tokens_i64(usage: Usage) -> i64 {
    usage_usize_to_i64(usage.total_tokens())
}

fn usage_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
