use super::*;

impl Database {
    pub async fn initialize(&self) -> Result<(), StoreError> {
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
}
