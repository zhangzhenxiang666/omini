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
        target_session_id: Option<&str>,
        records: Vec<domain::subagents::AgentRecord>,
    ) -> Result<(), CoreError> {
        let Some(session_id) = target_session_id else {
            return Ok(());
        };
        let Some(session) = self.cached_thread(session_id) else {
            return Ok(());
        };
        session.reload_subagent_registry().await?;
        session.broadcast_agent_management_updated(records)
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
        target_session_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let update = omini_core::save_project_agent(
            &self.cwd,
            runtime_contract::SaveProjectAgentCommand {
                source_kind: request.source_kind,
                original_agent_id: request.original_agent_id,
                draft: request.draft,
            },
        )?;
        self.refresh_target_thread_agents(target_session_id, update.records)
            .await
    }

    pub async fn delete_agent(
        &self,
        agent_id: &str,
        target_session_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let update = omini_core::delete_project_agent(
            &self.cwd,
            runtime_contract::DeleteProjectAgentCommand {
                agent_id: agent_id.to_string(),
            },
        )?;
        self.refresh_target_thread_agents(target_session_id, update.records)
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

#[cfg(test)]
mod tests {
    use crate::project::test_support::{
        project_manager_for, recv_runtime_event_kind, unique_temp_root,
    };
    use omini_protocol as client_proto;

    #[tokio::test]
    async fn save_agent_without_target_writes_file_without_spawning_runtime() {
        let temp = unique_temp_root("agent-save-no-target");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;

        manager
            .save_agent(
                client_proto::SaveAgentRequest {
                    source_kind: client_proto::AgentSourceKind::Project,
                    original_agent_id: None,
                    draft: client_proto::AgentDraft {
                        name: "cache-helper".to_string(),
                        description: "Use when checking cache-sensitive changes.".to_string(),
                        short_description: None,
                        instructions: "Inspect cache-sensitive changes.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                None,
            )
            .await
            .expect("agent should save");

        let agents = manager.list_agents().expect("agents should list");
        assert!(
            agents
                .records
                .iter()
                .any(|agent| agent.name == "cache-helper")
        );
        assert!(
            manager
                .threads
                .lock()
                .expect("sessions lock poisoned")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn save_agent_with_target_notifies_cached_session_agents() {
        let temp = unique_temp_root("agent-save-target");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;
        let session_id = manager
            .create_thread(client_proto::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .get_or_load_thread(&session_id)
            .await
            .expect("session should load");
        let mut events = session.subscribe();

        manager
            .save_agent(
                client_proto::SaveAgentRequest {
                    source_kind: client_proto::AgentSourceKind::Project,
                    original_agent_id: None,
                    draft: client_proto::AgentDraft {
                        name: "target-helper".to_string(),
                        description: "Use when testing target refresh.".to_string(),
                        short_description: None,
                        instructions: "Refresh me.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                Some(&session_id),
            )
            .await
            .expect("agent should save");

        let event = recv_runtime_event_kind(&mut events, "agent_management_updated").await;
        assert!(event.seq > 0);
        assert!(matches!(
            event.event.event,
            client_proto::TypedRuntimeEvent::AgentManagementUpdated { records }
                if records.iter().any(|record| record.name == "target-helper")
        ));
        session.shutdown().await.expect("session should shut down");
    }

    #[tokio::test]
    async fn save_agent_rejects_built_in_source_kind() {
        let temp = unique_temp_root("agent-save-built-in");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;

        let error = manager
            .save_agent(
                client_proto::SaveAgentRequest {
                    source_kind: client_proto::AgentSourceKind::BuiltIn,
                    original_agent_id: None,
                    draft: client_proto::AgentDraft {
                        name: "bad".to_string(),
                        description: "Bad built-in write.".to_string(),
                        short_description: None,
                        instructions: "Do not write.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                None,
            )
            .await
            .expect_err("built-in writes should fail");

        assert!(error.message().contains("内置 agent 不能写入"));
    }

    #[tokio::test]
    async fn delete_agent_requires_known_editable_record_id() {
        let temp = unique_temp_root("agent-delete-path-ownership");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;

        let arbitrary = cwd.join(".omini").join("agents").join("missing.md");
        let error = manager
            .delete_agent(&arbitrary.display().to_string(), None)
            .await
            .expect_err("unlisted path should not be deletable");
        assert!(error.message().contains("不存在或不可编辑"));

        manager
            .save_agent(
                client_proto::SaveAgentRequest {
                    source_kind: client_proto::AgentSourceKind::Project,
                    original_agent_id: None,
                    draft: client_proto::AgentDraft {
                        name: "deletable".to_string(),
                        description: "Use when testing deletion.".to_string(),
                        short_description: None,
                        instructions: "Delete me.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                None,
            )
            .await
            .expect("agent should save");
        let agents = manager.list_agents().expect("agents should list");
        let agent_id = agents
            .records
            .iter()
            .find(|agent| agent.name == "deletable")
            .expect("saved agent should be listed")
            .id
            .clone();

        manager
            .delete_agent(&agent_id, None)
            .await
            .expect("listed editable agent should delete");
        let agents = manager.list_agents().expect("agents should list");
        assert!(!agents.records.iter().any(|agent| agent.name == "deletable"));
    }
}
