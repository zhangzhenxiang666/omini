use crate::{
    event::bridge::models_response_from_settings,
    project::{ProjectManager, load_validated_config},
};
use omini_config::Settings;
use omini_core::CoreError;
use omini_domain as domain;
use omini_protocol as client_proto;

impl ProjectManager {
    fn project_runtime_config_response(
        &self,
    ) -> Result<client_proto::ProjectRuntimeConfigResponse, CoreError> {
        let settings = self.fresh_settings_with_state()?;
        let show_thinking_blocks = self
            .project
            .load_state()
            .map(|state| state.show_thinking_blocks)
            .unwrap_or(true);
        Ok(client_proto::ProjectRuntimeConfigResponse {
            context_window: settings.current_model_config().map(|model| model.limit),
            active_provider: settings.active_provider,
            model: settings.model,
            thinking_effort: settings.thinking_effort,
            show_thinking_blocks,
        })
    }

    pub fn fresh_settings_with_state(&self) -> Result<Settings, CoreError> {
        let config = load_validated_config(&self.root, &self.cwd)
            .map_err(|err| CoreError::config("failed to load effective config", err))?;
        let state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        let mut settings = config
            .to_settings(
                state.default_provider.as_deref(),
                state.default_model.as_deref(),
                state.thinking_effort,
            )
            .map_err(|error| CoreError::config("failed to build settings", error))?;
        settings.cwd = self.cwd.clone();
        Ok(settings)
    }

    pub fn list_models(&self) -> Result<client_proto::ModelsResponse, CoreError> {
        Ok(models_response_from_settings(
            &self.fresh_settings_with_state()?,
        ))
    }

    pub fn set_model(
        &self,
        request: client_proto::SetModelRequest,
    ) -> Result<client_proto::ProjectRuntimeConfigResponse, CoreError> {
        let settings = self.fresh_settings_with_state()?;
        let provider = settings.providers.get(&request.provider).ok_or_else(|| {
            CoreError::invalid_model_selection(format!(
                "Unknown provider profile: {}",
                request.provider
            ))
        })?;
        if !provider
            .models
            .iter()
            .any(|model| model.id == request.model)
        {
            return Err(CoreError::invalid_model_selection(format!(
                "Unknown model '{}' for provider '{}'",
                request.model, request.provider
            )));
        }
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        let provider = request.provider;
        let model = request.model;
        state.default_provider = Some(provider.clone());
        state.default_model = Some(model.clone());
        state.thinking_effort =
            settings.effective_thinking_effort_for(&provider, &model, request.thinking_effort);
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;
        self.project_runtime_config_response()
    }

    pub fn set_thinking_effort(
        &self,
        request: client_proto::SetThinkingEffortRequest,
    ) -> Result<client_proto::ProjectRuntimeConfigResponse, CoreError> {
        let settings = self.fresh_settings_with_state()?;
        if request.effort != domain::config::ThinkingEffort::None
            && !settings.current_model_supports_thinking()
        {
            return Err(CoreError::invalid_model_selection(format!(
                "Current model '{}' does not support thinking",
                settings.model
            )));
        }

        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        state.thinking_effort = settings.effective_current_thinking_effort(Some(request.effort));
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;
        self.project_runtime_config_response()
    }

    pub fn set_thinking_display(
        &self,
        request: client_proto::SetThinkingDisplayRequest,
    ) -> Result<client_proto::ProjectRuntimeConfigResponse, CoreError> {
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        state.show_thinking_blocks = request.show.unwrap_or(!state.show_thinking_blocks);
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;
        self.project_runtime_config_response()
    }
}

#[cfg(test)]
mod tests {
    use crate::project::test_support::{
        has_provider, project_manager_for, unique_temp_root, write_config, write_project_config,
    };
    use crate::project::{ProjectManager, load_validated_config};
    use crate::store::Database;
    use omini_config::OminiRoot;
    use omini_domain as domain;
    use omini_protocol as client_proto;
    use std::sync::Arc;

    #[tokio::test]
    async fn project_models_reflect_config_added_after_manager_creation() {
        let temp = unique_temp_root("project-models-refresh");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;

        let initial = manager.list_models().expect("models should load");
        assert!(!has_provider(&initial.providers, "anthropic"));

        write_config(&temp.path, true);

        let refreshed = manager.list_models().expect("models should reload");
        assert!(has_provider(&refreshed.providers, "anthropic"));
    }

    #[tokio::test]
    async fn project_models_include_project_level_config() {
        let temp = unique_temp_root("project-level-config");
        let cwd = temp.path.join("cwd");
        write_config(&temp.path, false);
        write_project_config(&cwd);

        let root = Arc::new(OminiRoot::from_path(temp.path.clone()));
        let config = load_validated_config(&root, &cwd).expect("config should load");
        let project = root
            .init_project(&cwd, &config)
            .expect("project should initialize");
        let db_path = root.path().join("omini.sqlite");
        let db = Database::open(&db_path)
            .await
            .expect("database should open");
        let manager = ProjectManager::new(root, cwd, project, Arc::new(db));

        let models = manager.list_models().expect("models should load");

        assert!(has_provider(&models.providers, "openai"));
        assert!(has_provider(&models.providers, "anthropic"));
        let anthropic = models
            .providers
            .iter()
            .find(|provider| provider.id == "anthropic")
            .expect("project provider should be listed");
        assert!(
            anthropic
                .models
                .iter()
                .any(|model| model.id == "claude-project")
        );
    }

    #[tokio::test]
    async fn set_project_model_clears_effort_for_non_thinking_model() {
        let temp = unique_temp_root("project-model");
        let cwd = temp.path.join("cwd");
        let (manager, project) = project_manager_for(&temp.path, &cwd).await;
        let mut state = project.load_state().expect("state should load");
        state.thinking_effort = Some(domain::config::ThinkingEffort::High);
        project.save_state(&state).expect("state should save");

        let response = manager
            .set_model(client_proto::SetModelRequest {
                provider: "openai".to_string(),
                model: "fast".to_string(),
                thinking_effort: Some(client_proto::ThinkingEffort::Medium),
            })
            .expect("model switch should succeed");

        assert_eq!(response.model, "fast");
        assert_eq!(response.thinking_effort, None);
        assert_eq!(
            project
                .load_state()
                .expect("state should load")
                .thinking_effort,
            None
        );
    }

    #[tokio::test]
    async fn set_project_thinking_effort_none_disables_thinking_model_effort() {
        let temp = unique_temp_root("project-effort-none");
        let cwd = temp.path.join("cwd");
        let (manager, project) = project_manager_for(&temp.path, &cwd).await;
        let mut state = project.load_state().expect("state should load");
        state.default_provider = Some("openai".to_string());
        state.default_model = Some("reasoner".to_string());
        state.thinking_effort = Some(domain::config::ThinkingEffort::High);
        project.save_state(&state).expect("state should save");

        let response = manager
            .set_thinking_effort(client_proto::SetThinkingEffortRequest {
                effort: client_proto::ThinkingEffort::None,
            })
            .expect("none effort should clear");

        assert_eq!(
            response.thinking_effort,
            Some(client_proto::ThinkingEffort::None)
        );
        assert_eq!(
            project
                .load_state()
                .expect("state should load")
                .thinking_effort,
            Some(domain::config::ThinkingEffort::None)
        );
    }
}
