use crate::project::ProjectManager;
use chrono::Utc;
use omini_core::CoreError;
use omini_protocol as client_proto;

impl ProjectManager {
    pub async fn list_agent_runs(
        &self,
        thread_id: &str,
        include_archived: bool,
    ) -> Result<client_proto::AgentRunsResponse, CoreError> {
        self.require_project_thread(thread_id).await?;
        self.db
            .list_agent_runs(thread_id, include_archived)
            .await
            .map(|runs| client_proto::AgentRunsResponse { runs })
            .map_err(|error| CoreError::persistence("failed to list AgentRuns", error.to_string()))
    }

    pub async fn get_agent_run_detail(
        &self,
        thread_id: &str,
        run_id: &str,
    ) -> Result<Option<client_proto::AgentRunDetailResponse>, CoreError> {
        self.require_project_thread(thread_id).await?;
        let Some(run) = self
            .db
            .get_agent_run(thread_id, run_id)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to load AgentRun", error.to_string())
            })?
        else {
            return Ok(None);
        };
        let steps = self.db.list_agent_steps(run_id).await.map_err(|error| {
            CoreError::persistence("failed to load AgentSteps", error.to_string())
        })?;
        let tool_uses = self
            .db
            .list_tool_use_executions(run_id)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to load ToolUses", error.to_string())
            })?;
        Ok(Some(client_proto::AgentRunDetailResponse {
            run,
            steps,
            tool_uses,
        }))
    }

    pub async fn archive_agent_run(
        &self,
        thread_id: &str,
        run_id: &str,
        archived: bool,
    ) -> Result<bool, CoreError> {
        self.require_project_thread(thread_id).await?;
        if self
            .db
            .get_agent_run(thread_id, run_id)
            .await
            .map_err(|error| CoreError::persistence("failed to load AgentRun", error.to_string()))?
            .is_none()
        {
            return Ok(false);
        }
        self.db
            .set_agent_run_archived(run_id, archived.then(Utc::now))
            .await
            .map_err(|error| {
                CoreError::persistence("failed to archive AgentRun", error.to_string())
            })
    }

    async fn require_project_thread(&self, thread_id: &str) -> Result<(), CoreError> {
        let thread = self
            .db
            .get_thread(thread_id)
            .await
            .map_err(|error| CoreError::persistence("failed to load thread", error.to_string()))?
            .ok_or(CoreError::ThreadNotFound)?;
        if thread.project_id != self.project_id {
            return Err(CoreError::ThreadNotFound);
        }
        Ok(())
    }
}
