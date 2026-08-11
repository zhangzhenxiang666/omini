use crate::{git, project::ProjectManager};
use omini_core::CoreError;
use omini_protocol as client_proto;

impl ProjectManager {
    pub async fn open_response(
        &self,
        project: client_proto::ProjectSummary,
    ) -> Result<client_proto::OpenProjectResponse, CoreError> {
        let settings = self.fresh_settings_with_state()?;
        let threads = self.list_threads().await?.threads;
        let context_window = settings.current_model_config().map(|model| model.limit);
        let mcp_server_count = settings
            .mcp_servers
            .values()
            .filter(|server| server.enabled)
            .count();
        let has_project_instructions = self
            .cwd
            .join("AGENTS.md")
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0);
        let show_thinking_blocks = self
            .project
            .load_state()
            .map(|state| state.show_thinking_blocks)
            .unwrap_or(true);
        let agents = omini_core::project_agents_snapshot(&settings)
            .records
            .into_iter()
            .map(|agent| client_proto::AgentSummary {
                name: agent.name,
                description: agent.description,
                short_description: agent.short_description,
                location: agent
                    .path
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<built-in>".to_string()),
            })
            .collect();
        let skills = omini_core::project_skill_summaries(&self.cwd)
            .into_iter()
            .map(|skill| client_proto::SkillSummary {
                name: skill.name,
                description: skill.description,
                short_description: skill.short_description,
            })
            .collect();

        Ok(client_proto::OpenProjectResponse {
            project,
            threads,
            active_provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort,
            context_window,
            mcp_server_count,
            has_project_instructions,
            show_thinking_blocks,
            agents,
            skills,
            git_branch: git::detect_git_branch(&self.cwd),
        })
    }
}
