use crate::{
    event::bridge::agents_response_from_runtime_snapshot,
    project::{
        ProjectManager,
        model_selection::{EffortSelection, ModelSelection},
    },
};
use omini_core::CoreError;
use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;

impl ProjectManager {
    async fn refresh_target_thread_agents(
        &self,
        target_thread_id: Option<&str>,
        records: Vec<domain::subagents::AgentRecord>,
    ) -> Result<(), CoreError> {
        let Some(thread_id) = target_thread_id else {
            return Ok(());
        };
        let Some(thread) = self.cached_thread(thread_id) else {
            return Ok(());
        };
        thread.reload_subagent_registry().await?;
        thread.broadcast_agent_management_updated(records)
    }

    pub fn list_agents(&self) -> Result<client_proto::AgentsResponse, CoreError> {
        let settings = self.fresh_settings_with_state()?;
        Ok(agents_response_from_runtime_snapshot(
            omini_core::project_agents_snapshot(&settings),
        ))
    }

    pub async fn save_agent(
        &self,
        request: client_proto::SaveAgentRequest,
        target_thread_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let update = omini_core::save_project_agent(
            &self.cwd,
            runtime_contract::SaveProjectAgentCommand {
                source_kind: request.source_kind,
                original_agent_id: request.original_agent_id,
                draft: request.draft,
            },
        )?;
        self.refresh_target_thread_agents(target_thread_id, update.records)
            .await
    }

    pub async fn delete_agent(
        &self,
        agent_id: &str,
        target_thread_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let update = omini_core::delete_project_agent(
            &self.cwd,
            runtime_contract::DeleteProjectAgentCommand {
                agent_id: agent_id.to_string(),
            },
        )?;
        self.refresh_target_thread_agents(target_thread_id, update.records)
            .await
    }

    pub async fn generate_agent(
        &self,
        request: client_proto::GenerateAgentRequest,
    ) -> Result<client_proto::GenerateAgentResponse, CoreError> {
        let settings = self.settings_for_model_selection(
            ModelSelection::Exact {
                provider: &request.provider,
                model: &request.model,
            },
            EffortSelection::ClientRequest(request.thinking_effort),
        )?;
        let draft =
            omini_core::generate_project_agent_draft(&settings, &request.description).await?;
        Ok(client_proto::GenerateAgentResponse { draft })
    }
}
