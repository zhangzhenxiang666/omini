use crate::{
    event::bridge::models_response_from_settings,
    project::{ProjectManager, load_validated_config},
};
use omini_config::{ModelSelection, Settings};
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
        let model = settings.active_model();
        Ok(client_proto::ProjectRuntimeConfigResponse {
            context_window: Some(model.context_window),
            active_provider: model.provider_id.clone(),
            model: model.model_id.clone(),
            thinking_effort: model.thinking_effort,
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
        config
            .to_settings(
                state.default_provider.as_deref(),
                state.default_model.as_deref(),
                state.thinking_effort,
                &self.cwd,
            )
            .map_err(|error| CoreError::config("failed to build settings", error))
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
        let selected = settings
            .resolve_model(&ModelSelection {
                active_provider: request.provider,
                model: request.model,
                thinking_effort: request.thinking_effort,
            })
            .map_err(|error| CoreError::invalid_model_selection(error.to_string()))?;
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        state.default_provider = Some(selected.provider_id);
        state.default_model = Some(selected.model_id);
        state.thinking_effort = selected.thinking_effort;
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
        let current = settings.active_model();
        if request.effort != domain::config::ThinkingEffort::None && !current.capabilities.thinking
        {
            return Err(CoreError::invalid_model_selection(format!(
                "Current model '{}' does not support thinking",
                current.model_id
            )));
        }
        let selected = settings
            .resolve_model(&ModelSelection {
                active_provider: current.provider_id.clone(),
                model: current.model_id.clone(),
                thinking_effort: Some(request.effort),
            })
            .map_err(|error| CoreError::invalid_model_selection(error.to_string()))?;

        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        state.thinking_effort = selected.thinking_effort;
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
