use super::service::AgentRuntime;
use super::*;

impl AgentRuntime {
    pub(super) async fn save_agent(
        &mut self,
        source_kind: crate::subagents::AgentSourceKind,
        original_path: Option<&std::path::Path>,
        draft: &crate::subagents::AgentDraft,
    ) {
        if crate::subagents::agent_name_exists(&self.settings.cwd, &draft.name, original_path) {
            self.send_event(RuntimeToUiEvent::error(format!(
                "agent '{}' 已存在",
                draft.name
            )))
            .await;
            return;
        }
        match crate::subagents::write_agent_file(&self.settings.cwd, source_kind, draft) {
            Ok(written_path) => {
                if let Some(path) = original_path
                    && path != written_path
                {
                    let _ = crate::subagents::delete_agent_file(path);
                }
                self.refresh_agents_after_change().await;
                self.send_event(RuntimeToUiEvent::notice(format!(
                    "agent '{}' 已保存",
                    draft.name
                )))
                .await;
            }
            Err(e) => self.send_event(RuntimeToUiEvent::error(e)).await,
        }
    }

    pub(super) async fn delete_agent(&mut self, path: &std::path::Path) {
        match crate::subagents::delete_agent_file(path) {
            Ok(()) => {
                self.refresh_agents_after_change().await;
                self.send_event(RuntimeToUiEvent::notice("agent 已删除".to_string()))
                    .await;
            }
            Err(e) => self.send_event(RuntimeToUiEvent::error(e)).await,
        }
    }

    async fn refresh_agents_after_change(&mut self) {
        let registry = self.capabilities.reload_subagents(&self.settings);
        let skill_registry = self.capabilities.skill_registry();
        self.settings.system_prompt = Some(crate::prompts::build_system_prompt_with_capabilities(
            &self.settings,
            &registry.summaries(),
            &skill_registry.injected_summaries(),
            self.active_profile(),
        ));
        self.send_event(RuntimeToUiEvent::AgentList(registry.summaries()))
            .await;
        self.send_event(RuntimeToUiEvent::AgentManagementUpdated {
            records: crate::subagents::list_agent_records(&self.settings.cwd),
        })
        .await;
    }

    pub(super) async fn generate_agent(
        &mut self,
        source_kind: crate::subagents::AgentSourceKind,
        description: &str,
        tools: Vec<String>,
        disallow_tools: Vec<String>,
        model: Option<String>,
    ) {
        match crate::subagents::generate_agent_draft(
            &self.llm_client,
            &self.settings,
            description,
            tools,
            disallow_tools,
            model,
        )
        .await
        {
            Ok(draft) => {
                self.send_event(RuntimeToUiEvent::AgentGenerated { source_kind, draft })
                    .await;
            }
            Err(e) => {
                self.send_event(RuntimeToUiEvent::AgentGenerateFailed { message: e })
                    .await;
            }
        }
    }
}
