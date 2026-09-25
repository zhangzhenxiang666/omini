use super::*;
use omini_domain::agent_run::{
    AgentRunKind, AgentRunSnapshot, AgentRunStatus, AgentStepSnapshot, AgentStepStatus,
    ToolUseExecutionSnapshot, ToolUseStatus,
};

#[derive(Debug, FromRow)]
struct AgentRunRow {
    id: String,
    thread_id: String,
    parent_run_id: Option<String>,
    kind: String,
    status: String,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    total_tokens: i64,
    archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromRow)]
struct AgentStepRow {
    id: String,
    run_id: String,
    step_no: i64,
    status: String,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    input_tokens: i64,
    output_tokens: i64,
}

#[derive(Debug, FromRow)]
struct ToolUseExecutionRow {
    id: String,
    step_id: String,
    name: String,
    input_json: String,
    status: String,
    updated_at: DateTime<Utc>,
}

impl Database {
    pub async fn create_agent_run(&self, run: &AgentRunSnapshot) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO agent_run(id, thread_id, parent_run_id, kind, status, created_at, started_at, finished_at, total_tokens, archived_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&run.id)
        .bind(&run.thread_id)
        .bind(&run.parent_run_id)
        .bind(run.kind.as_str())
        .bind(run.status.as_str())
        .bind(run.created_at)
        .bind(run.started_at)
        .bind(run.finished_at)
        .bind(run.total_tokens)
        .bind(run.archived_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_agent_run(
        &self,
        run_id: &str,
        status: AgentRunStatus,
        started_at: Option<DateTime<Utc>>,
        finished_at: Option<DateTime<Utc>>,
        add_tokens: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE agent_run SET status = ?, started_at = COALESCE(started_at, ?), finished_at = COALESCE(?, finished_at), total_tokens = total_tokens + ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(started_at)
        .bind(finished_at)
        .bind(add_tokens)
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_agent_step(&self, step: &AgentStepSnapshot) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO agent_step(id, run_id, step_no, status, started_at, finished_at, input_tokens, output_tokens)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, finished_at = excluded.finished_at,
                input_tokens = excluded.input_tokens, output_tokens = excluded.output_tokens",
        )
        .bind(&step.id)
        .bind(&step.run_id)
        .bind(i64::from(step.step_no))
        .bind(step.status.as_str())
        .bind(step.started_at)
        .bind(step.finished_at)
        .bind(step.input_tokens)
        .bind(step.output_tokens)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_agent_step(
        &self,
        step_id: &str,
        status: AgentStepStatus,
        finished_at: Option<DateTime<Utc>>,
        add_input_tokens: i64,
        add_output_tokens: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE agent_step SET status = ?, finished_at = COALESCE(?, finished_at), input_tokens = input_tokens + ?, output_tokens = output_tokens + ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(finished_at)
        .bind(add_input_tokens)
        .bind(add_output_tokens)
        .bind(step_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_tool_use_execution(
        &self,
        item: &ToolUseExecutionSnapshot,
        status: ToolUseStatus,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO tool_use_execution(id, step_id, name, input_json, status, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, updated_at = excluded.updated_at",
        )
        .bind(&item.id)
        .bind(&item.step_id)
        .bind(&item.name)
        .bind(serde_json::to_string(&item.input)?)
        .bind(status.as_str())
        .bind(item.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_agent_run_archived(
        &self,
        run_id: &str,
        archived_at: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "UPDATE agent_run SET archived_at = ? WHERE id = ? AND status IN ('completed', 'failed', 'cancelled', 'interrupted')",
        )
        .bind(archived_at)
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_agent_runs(
        &self,
        thread_id: &str,
        include_archived: bool,
    ) -> Result<Vec<AgentRunSnapshot>, StoreError> {
        let rows = if include_archived {
            sqlx::query_as::<_, AgentRunRow>(
                "SELECT * FROM agent_run WHERE thread_id = ? ORDER BY created_at, id",
            )
            .bind(thread_id)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, AgentRunRow>(
                "SELECT * FROM agent_run WHERE thread_id = ? AND archived_at IS NULL ORDER BY created_at, id",
            )
            .bind(thread_id)
            .fetch_all(&self.pool)
            .await?
        };
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn get_agent_run(
        &self,
        thread_id: &str,
        run_id: &str,
    ) -> Result<Option<AgentRunSnapshot>, StoreError> {
        sqlx::query_as::<_, AgentRunRow>("SELECT * FROM agent_run WHERE thread_id = ? AND id = ?")
            .bind(thread_id)
            .bind(run_id)
            .fetch_optional(&self.pool)
            .await?
            .map(TryInto::try_into)
            .transpose()
    }

    pub async fn list_agent_steps(
        &self,
        run_id: &str,
    ) -> Result<Vec<AgentStepSnapshot>, StoreError> {
        let rows = sqlx::query_as::<_, AgentStepRow>(
            "SELECT * FROM agent_step WHERE run_id = ? ORDER BY step_no",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn list_tool_use_executions(
        &self,
        run_id: &str,
    ) -> Result<Vec<ToolUseExecutionSnapshot>, StoreError> {
        let rows = sqlx::query_as::<_, ToolUseExecutionRow>(
            "SELECT t.* FROM tool_use_execution t JOIN agent_step s ON s.id = t.step_id WHERE s.run_id = ? ORDER BY s.step_no, t.updated_at, t.id",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

impl TryFrom<AgentRunRow> for AgentRunSnapshot {
    type Error = StoreError;

    fn try_from(row: AgentRunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            thread_id: row.thread_id,
            parent_run_id: row.parent_run_id,
            kind: match row.kind.as_str() {
                "agent" => AgentRunKind::Agent,
                "bash" => AgentRunKind::Bash,
                value => {
                    return Err(StoreError::InvalidData(format!(
                        "unknown run kind '{value}'"
                    )));
                }
            },
            status: parse_run_status(&row.status)?,
            created_at: row.created_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
            total_tokens: row.total_tokens,
            archived_at: row.archived_at,
        })
    }
}

impl TryFrom<AgentStepRow> for AgentStepSnapshot {
    type Error = StoreError;

    fn try_from(row: AgentStepRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            run_id: row.run_id,
            step_no: u32::try_from(row.step_no)
                .map_err(|_| StoreError::InvalidData("invalid AgentStep number".to_string()))?,
            status: parse_step_status(&row.status)?,
            started_at: row.started_at,
            finished_at: row.finished_at,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
        })
    }
}

impl TryFrom<ToolUseExecutionRow> for ToolUseExecutionSnapshot {
    type Error = StoreError;

    fn try_from(row: ToolUseExecutionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            step_id: row.step_id,
            name: row.name,
            input: serde_json::from_str(&row.input_json)?,
            status: parse_tool_use_status(&row.status)?,
            updated_at: row.updated_at,
        })
    }
}

fn parse_run_status(value: &str) -> Result<AgentRunStatus, StoreError> {
    match value {
        "queued" => Ok(AgentRunStatus::Queued),
        "running" => Ok(AgentRunStatus::Running),
        "waiting_approval" => Ok(AgentRunStatus::WaitingApproval),
        "completed" => Ok(AgentRunStatus::Completed),
        "failed" => Ok(AgentRunStatus::Failed),
        "cancelled" => Ok(AgentRunStatus::Cancelled),
        "interrupted" => Ok(AgentRunStatus::Interrupted),
        _ => Err(StoreError::InvalidData(format!(
            "unknown run status '{value}'"
        ))),
    }
}

fn parse_step_status(value: &str) -> Result<AgentStepStatus, StoreError> {
    match value {
        "running" => Ok(AgentStepStatus::Running),
        "waiting_approval" => Ok(AgentStepStatus::WaitingApproval),
        "completed" => Ok(AgentStepStatus::Completed),
        "failed" => Ok(AgentStepStatus::Failed),
        "cancelled" => Ok(AgentStepStatus::Cancelled),
        "interrupted" => Ok(AgentStepStatus::Interrupted),
        _ => Err(StoreError::InvalidData(format!(
            "unknown step status '{value}'"
        ))),
    }
}

fn parse_tool_use_status(value: &str) -> Result<ToolUseStatus, StoreError> {
    match value {
        "pending" => Ok(ToolUseStatus::Pending),
        "waiting_approval" => Ok(ToolUseStatus::WaitingApproval),
        "running" => Ok(ToolUseStatus::Running),
        "completed" => Ok(ToolUseStatus::Completed),
        "failed" => Ok(ToolUseStatus::Failed),
        "cancelled" => Ok(ToolUseStatus::Cancelled),
        _ => Err(StoreError::InvalidData(format!(
            "unknown ToolUse status '{value}'"
        ))),
    }
}
