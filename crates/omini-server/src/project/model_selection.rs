use crate::project::ProjectManager;
use omini_config::Settings;
use omini_core::CoreError;
use omini_domain as domain;

impl ProjectManager {
    pub(super) fn settings_for_model_selection(
        &self,
        model: ModelSelection<'_>,
        effort: EffortSelection,
    ) -> Result<Settings, CoreError> {
        let mut settings = self.fresh_settings_with_state()?;

        match model {
            ModelSelection::ProjectDefault => {}
            ModelSelection::Exact { provider, model } => {
                apply_provider(&mut settings, provider)?;
                apply_model(&mut settings, model)?;
            }
            ModelSelection::PartialOverlay { provider, model } => {
                if let Some(provider) = provider {
                    apply_provider(&mut settings, provider)?;
                }
                if let Some(model) = model {
                    apply_model(&mut settings, model)?;
                }
            }
        }

        match effort {
            EffortSelection::InheritProject => {}
            EffortSelection::ClientRequest(Some(effort)) => {
                if effort != domain::config::ThinkingEffort::None
                    && !settings.current_model_supports_thinking()
                {
                    return Err(CoreError::invalid_model_selection(format!(
                        "Model '{}' does not support thinking",
                        settings.model
                    )));
                }
                settings.thinking_effort = settings.effective_current_thinking_effort(Some(effort));
            }
            EffortSelection::ClientRequest(None) => {}
            EffortSelection::PersistedLenient(effort) => {
                settings.thinking_effort = settings.effective_current_thinking_effort(effort);
            }
        }

        settings.normalize_current_thinking_effort();
        Ok(settings)
    }
}

pub(super) enum ModelSelection<'a> {
    ProjectDefault,
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
    InheritProject,
    ClientRequest(Option<domain::config::ThinkingEffort>),
    PersistedLenient(Option<domain::config::ThinkingEffort>),
}

fn apply_provider(settings: &mut Settings, provider: &str) -> Result<(), CoreError> {
    let profile = settings.providers.get(provider).ok_or_else(|| {
        CoreError::invalid_model_selection(format!("Unknown provider profile: {provider}"))
    })?;

    settings.active_provider = provider.to_string();
    settings.api_key = profile.api_key.clone();
    settings.base_url = profile.base_url.clone();
    settings.endpoint = profile.endpoint;

    Ok(())
}

fn apply_model(settings: &mut Settings, model: &str) -> Result<(), CoreError> {
    let provider = settings
        .providers
        .get(&settings.active_provider)
        .ok_or_else(|| {
            CoreError::invalid_model_selection(format!(
                "Unknown provider profile: {}",
                settings.active_provider
            ))
        })?;

    if !provider
        .models
        .iter()
        .any(|candidate| candidate.id == model)
    {
        return Err(CoreError::invalid_model_selection(format!(
            "Unknown model '{}' for provider '{}'",
            model, settings.active_provider
        )));
    }

    settings.model = model.to_string();
    Ok(())
}
