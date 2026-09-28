use crate::project::ProjectManager;
use jiff::Timestamp;
use omini_core::CoreError;
use omini_protocol as client_proto;

impl ProjectManager {
    /// 查询属于主线程的子任务，供主线程控制入口解析子 Run 的真实线程。
    pub async fn get_owned_task(
        &self,
        owner_thread_id: &str,
        task_id: &str,
    ) -> Result<Option<crate::store::AgentTask>, CoreError> {
        self.require_project_thread(owner_thread_id).await?;
        self.db
            .get_owned_task(owner_thread_id, task_id)
            .await
            .map_err(|error| CoreError::persistence("failed to load agent task", error.to_string()))
    }

    pub async fn list_agent_runs(
        &self,
        thread_id: &str,
        include_archived: bool,
    ) -> Result<client_proto::AgentRunsResponse, CoreError> {
        self.require_project_thread(thread_id).await?;
        self.db
            .list_agent_runs(thread_id, include_archived)
            .await
            .map(|runs| client_proto::AgentRunsResponse {
                runs: runs.into_iter().map(Into::into).collect(),
            })
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
            run: run.into(),
            steps: steps.into_iter().map(Into::into).collect(),
            tool_uses: tool_uses.into_iter().map(Into::into).collect(),
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
            .set_agent_run_archived(run_id, archived.then(Timestamp::now))
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
