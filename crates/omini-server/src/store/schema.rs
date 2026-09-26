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
            "CREATE TABLE IF NOT EXISTS agent_run (
                id              TEXT PRIMARY KEY,
                thread_id       TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                parent_run_id   TEXT REFERENCES agent_run(id) ON DELETE CASCADE,
                kind            TEXT NOT NULL CHECK (kind IN ('agent', 'bash')),
                status          TEXT NOT NULL CHECK (status IN ('queued', 'running', 'waiting_approval', 'completed', 'failed', 'cancelled', 'interrupted')),
                created_at      TEXT NOT NULL,
                started_at      TEXT,
                finished_at     TEXT,
                total_tokens    INTEGER NOT NULL DEFAULT 0,
                archived_at     TEXT
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_step (
                id              TEXT PRIMARY KEY,
                run_id          TEXT NOT NULL REFERENCES agent_run(id) ON DELETE CASCADE,
                step_no         INTEGER NOT NULL,
                status          TEXT NOT NULL CHECK (status IN ('running', 'waiting_approval', 'completed', 'failed', 'cancelled', 'interrupted')),
                started_at      TEXT NOT NULL,
                finished_at     TEXT,
                input_tokens    INTEGER NOT NULL DEFAULT 0,
                output_tokens   INTEGER NOT NULL DEFAULT 0,
                UNIQUE (run_id, step_no)
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tool_use_execution (
                id              TEXT PRIMARY KEY,
                step_id         TEXT NOT NULL REFERENCES agent_step(id) ON DELETE CASCADE,
                name            TEXT NOT NULL,
                input_json      TEXT NOT NULL,
                status          TEXT NOT NULL CHECK (status IN ('pending', 'waiting_approval', 'running', 'completed', 'failed', 'cancelled')),
                updated_at      TEXT NOT NULL
            )",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_task (
                task_id                 TEXT PRIMARY KEY,
                owner_thread_id         TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                agent_thread_id         TEXT NOT NULL UNIQUE REFERENCES thread(id) ON DELETE CASCADE,
                parent_run_id           TEXT REFERENCES agent_run(id) ON DELETE SET NULL,
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

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS background_task (
                task_id          TEXT PRIMARY KEY,
                owner_thread_id  TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                kind             TEXT NOT NULL CHECK (kind IN ('sub_agent', 'bash')),
                title            TEXT NOT NULL,
                status           TEXT NOT NULL CHECK (status IN ('running', 'cancelling', 'completed', 'failed', 'cancelled', 'interrupted')),
                result_summary  TEXT,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                completed_at     TEXT,
                notification_delivered INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&mut *tx)
        .await?;

        let has_task_notification_state: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('background_task') WHERE name = 'notification_delivered'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_task_notification_state == 0 {
            sqlx::query(
                "ALTER TABLE background_task ADD COLUMN notification_delivered INTEGER NOT NULL DEFAULT 0",
            )
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "INSERT OR IGNORE INTO background_task(
                task_id, owner_thread_id, kind, title, status, result_summary,
                created_at, updated_at, completed_at, notification_delivered
            ) SELECT task_id, owner_thread_id, 'sub_agent', title, status,
                json_extract(result_json, '$.output'), created_at, updated_at, completed_at,
                notification_delivered
              FROM agent_task",
        )
        .execute(&mut *tx)
        .await?;

        let has_parent_run_id: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('agent_task') WHERE name = 'parent_run_id'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if has_parent_run_id == 0 {
            sqlx::query("ALTER TABLE agent_task ADD COLUMN parent_run_id TEXT REFERENCES agent_run(id) ON DELETE SET NULL")
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS attachment (
                id              TEXT PRIMARY KEY,
                thread_id       TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
                original_name   TEXT NOT NULL,
                mime_type       TEXT NOT NULL,
                size            INTEGER NOT NULL,
                sha256          TEXT NOT NULL,
                relative_path   TEXT NOT NULL,
                created_at      TEXT NOT NULL
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
            "CREATE INDEX IF NOT EXISTS idx_background_task_owner ON background_task(owner_thread_id, updated_at DESC)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_agent_run_thread ON agent_run(thread_id, created_at)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_agent_step_run ON agent_step(run_id, step_no)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_attachment_thread ON attachment(thread_id, created_at)",
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE agent_task SET status = 'interrupted', completed_at = COALESCE(completed_at, ?), updated_at = ? WHERE status IN ('running', 'cancelling')",
        )
        .bind(Utc::now())
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE background_task SET status = 'interrupted', completed_at = COALESCE(completed_at, ?), updated_at = ? WHERE status IN ('running', 'cancelling')",
        )
        .bind(Utc::now())
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_run SET status = 'interrupted', finished_at = COALESCE(finished_at, ?) WHERE id IN (SELECT task_id FROM agent_task WHERE status = 'interrupted') AND status IN ('queued', 'running', 'waiting_approval')",
        )
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
        let now = Utc::now();
        sqlx::query(
            "UPDATE agent_run SET status = 'interrupted', finished_at = COALESCE(finished_at, ?) WHERE parent_run_id IS NULL AND status = 'running'",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_run SET status = 'cancelled', finished_at = COALESCE(finished_at, ?) WHERE parent_run_id IS NOT NULL AND status IN ('queued', 'running', 'waiting_approval')",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_run SET status = 'interrupted', finished_at = COALESCE(finished_at, ?) WHERE parent_run_id IS NULL AND status = 'queued'",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_step SET status = 'interrupted', finished_at = COALESCE(finished_at, ?) WHERE status = 'running' AND run_id IN (SELECT id FROM agent_run WHERE status IN ('interrupted', 'cancelled'))",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_step SET status = 'waiting_approval' WHERE status = 'running' AND run_id IN (SELECT id FROM agent_run WHERE status = 'waiting_approval' AND parent_run_id IS NULL)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_step SET status = 'cancelled', finished_at = COALESCE(finished_at, ?) WHERE status = 'waiting_approval' AND run_id IN (SELECT id FROM agent_run WHERE parent_run_id IS NOT NULL AND status = 'cancelled')",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE tool_use_execution SET status = 'cancelled', updated_at = ? WHERE status IN ('pending', 'waiting_approval', 'running') AND step_id IN (SELECT s.id FROM agent_step s JOIN agent_run r ON r.id = s.run_id WHERE r.parent_run_id IS NOT NULL AND r.status = 'cancelled')",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}
