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
