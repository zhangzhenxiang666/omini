use crate::project::ProjectManager;
use omini_config::{ModelSelection as ConfigModelSelection, Settings};
use omini_core::CoreError;
use omini_domain as domain;

impl ProjectManager {
    pub(super) fn settings_for_model_selection(
        &self,
        model: ModelSelection<'_>,
        effort: EffortSelection,
    ) -> Result<Settings, CoreError> {
        let mut settings = self.fresh_settings_with_state()?;
        let current = settings.active_model();
        let (provider, requested_model) = match model {
            ModelSelection::Exact { provider, model } => {
                (provider.to_string(), Some(model.to_string()))
            }
            ModelSelection::PartialOverlay { provider, model } => (
                provider.unwrap_or(&current.provider_id).to_string(),
                model.map(str::to_string),
            ),
        };
        let model = match requested_model {
            Some(model) => model,
            None if provider == current.provider_id => current.model_id.clone(),
            None => settings
                .resolved_config()
                .providers
                .get(&provider)
                .and_then(|provider| provider.models.first())
                .map(|(id, _)| id.clone())
                .ok_or_else(|| {
                    CoreError::invalid_model_selection(format!(
                        "Unknown provider profile: {provider}"
                    ))
                })?,
        };
        let thinking_effort = match effort {
            EffortSelection::ClientRequest(Some(effort)) => {
                if effort != domain::config::ThinkingEffort::None {
                    let candidate = settings
                        .resolved_config()
                        .model(&provider, &model)
                        .map_err(|error| CoreError::invalid_model_selection(error.to_string()))?;
                    if !candidate.capabilities.thinking {
                        return Err(CoreError::invalid_model_selection(format!(
                            "Model '{model}' does not support thinking"
                        )));
                    }
                }
                Some(effort)
            }
            EffortSelection::ClientRequest(None) => current.thinking_effort,
        };
        settings
            .select_model(ConfigModelSelection {
                active_provider: provider,
                model,
                thinking_effort,
            })
            .map_err(|error| CoreError::invalid_model_selection(error.to_string()))?;
        Ok(settings)
    }
}

pub(super) enum ModelSelection<'a> {
    Exact {
        provider: &'a str,
        model: &'a str,
    },
    PartialOverlay {
        provider: Option<&'a str>,
        model: Option<&'a str>,
    },
}

pub(super) enum EffortSelection {
    ClientRequest(Option<domain::config::ThinkingEffort>),
}
