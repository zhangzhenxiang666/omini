use crate::config::project::ProjectDir;
use crate::types::config::Settings;
use omini_domain::config::ThinkingEffort;
use omini_domain::events::{ActiveProfile, SessionUsageSnapshot};
use omini_provider_api::LlmClient;
use omini_runtime_api::RuntimeToServerEvent;
use omini_runtime_api::persistence::RuntimePersistenceEvent;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use super::capabilities::CapabilityStore;

pub(super) async fn toggle_active_profile(
    active_profile: &mut ActiveProfile,
    settings: &mut Settings,
    capabilities: &CapabilityStore,
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
) {
    let next = match *active_profile {
        ActiveProfile::Main => ActiveProfile::Auto,
        ActiveProfile::Auto => ActiveProfile::Main,
        ActiveProfile::Plan => return,
    };
    *active_profile = next;
    rebuild_system_prompt(settings, capabilities, next);
    let _ = event_tx
        .send(RuntimeToServerEvent::ActiveProfileChanged(next))
        .await;
}

pub(super) async fn reject_request(event_tx: &mpsc::Sender<RuntimeToServerEvent>) {
    let _ = event_tx
        .send(RuntimeToServerEvent::error(
            "Cannot handle this request while a run is active".to_string(),
        ))
        .await;
}

pub(super) fn rebuild_system_prompt(
    settings: &mut Settings,
    capabilities: &CapabilityStore,
    active_profile: ActiveProfile,
) {
    let subagent_registry = capabilities.subagent_registry();
    let skill_registry = capabilities.skill_registry();
    settings.system_prompt = Some(crate::prompts::build_system_prompt_with_capabilities(
        settings,
        &subagent_registry.summaries(),
        &skill_registry.injected_summaries(),
        active_profile,
    ));
}

pub(super) fn current_context_window(settings: &Settings) -> Option<u32> {
    settings
        .providers
        .get(&settings.active_provider)
        .and_then(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.id == settings.model)
                .map(|model| model.limit)
        })
}

pub(super) struct ModelSelection<'a> {
    pub(super) provider: &'a str,
    pub(super) model: &'a str,
    pub(super) thinking_effort: Option<ThinkingEffort>,
}

pub(super) struct RuntimeSinks<'a> {
    pub(super) event_tx: &'a mpsc::Sender<RuntimeToServerEvent>,
    pub(super) persistence_tx: &'a mpsc::Sender<RuntimePersistenceEvent>,
    pub(super) usage_state: &'a Arc<Mutex<SessionUsageSnapshot>>,
}

pub(super) async fn apply_model_selection(
    settings: &mut Settings,
    llm_client: &mut LlmClient,
    project: &ProjectDir,
    session_id: Option<&str>,
    selection: ModelSelection<'_>,
    sinks: RuntimeSinks<'_>,
) {
    let provider = selection.provider;
    let model = selection.model;
    if let Some(profile) = settings.providers.get(provider) {
        let thinking_effort =
            settings.effective_thinking_effort_for(provider, model, selection.thinking_effort);
        settings.active_provider = provider.to_string();
        settings.model = model.to_string();
        settings.thinking_effort = thinking_effort;
        settings.api_key = profile.api_key.clone();
        settings.base_url = profile.base_url.clone();
        settings.endpoint = profile.endpoint;

        *llm_client = LlmClient::new(
            profile.endpoint,
            profile.api_key.clone(),
            profile.base_url.clone(),
        );

        if let Some(sid) = session_id {
            let te = thinking_effort.map(|t| t.to_string());
            let _ = sinks
                .persistence_tx
                .send(RuntimePersistenceEvent::UpdateSessionConfig {
                    session_id: sid.to_string(),
                    provider: provider.to_string(),
                    model: model.to_string(),
                    thinking_effort: te,
                })
                .await;
        } else if let Ok(mut state) = project.load_state() {
            state.default_provider = Some(provider.to_string());
            state.default_model = Some(model.to_string());
            state.thinking_effort = thinking_effort;
            let _ = project.save_state(&state);
        }

        let _ = sinks
            .event_tx
            .send(RuntimeToServerEvent::ModelChanged {
                provider: provider.to_string(),
                model: model.to_string(),
                thinking_effort: settings.thinking_effort,
                context_window: current_context_window(settings),
            })
            .await;
        send_usage_snapshot(sinks.event_tx, sinks.usage_state, settings).await;
    } else {
        let _ = sinks
            .event_tx
            .send(RuntimeToServerEvent::error(format!(
                "提供商 '{provider}' 不存在"
            )))
            .await;
    }
}

pub(super) async fn apply_thinking_effort(
    settings: &mut Settings,
    project: &ProjectDir,
    session_id: Option<&str>,
    effort: ThinkingEffort,
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    if effort != ThinkingEffort::None {
        let supports_thinking = settings
            .providers
            .get(&settings.active_provider)
            .and_then(|profile| {
                profile
                    .models
                    .iter()
                    .find(|model| model.id == settings.model)
            })
            .is_some_and(|model| model.thinking);
        if !supports_thinking {
            let _ = event_tx
                .send(RuntimeToServerEvent::error(format!(
                    "当前模型 '{}' 不支持思考模式",
                    settings.model
                )))
                .await;
            return;
        }
    }

    let effective_effort = settings.effective_current_thinking_effort(Some(effort));
    if let Some(sid) = session_id {
        if persistence_tx
            .send(RuntimePersistenceEvent::UpdateSessionThinkingEffort {
                session_id: sid.to_string(),
                thinking_effort: effective_effort.map(|effort| effort.to_string()),
            })
            .await
            .is_err()
        {
            let _ = event_tx
                .send(RuntimeToServerEvent::error("更新思考程度失败".to_string()))
                .await;
            return;
        }
    } else {
        let mut state = match project.load_state() {
            Ok(state) => state,
            Err(e) => {
                let _ = event_tx
                    .send(RuntimeToServerEvent::error(format!(
                        "读取项目状态失败: {e}"
                    )))
                    .await;
                return;
            }
        };
        state.thinking_effort = effective_effort;
        if let Err(e) = project.save_state(&state) {
            let _ = event_tx
                .send(RuntimeToServerEvent::error(format!(
                    "保存项目状态失败: {e}"
                )))
                .await;
            return;
        }
    }

    settings.thinking_effort = effective_effort;
    let _ = event_tx
        .send(RuntimeToServerEvent::ModelChanged {
            provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort,
            context_window: current_context_window(settings),
        })
        .await;
}

async fn send_usage_snapshot(
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
    settings: &Settings,
) {
    let context_window = current_context_window(settings);
    let mut snapshot = usage_state.lock().await;
    snapshot.context_window = context_window;
    let event = RuntimeToServerEvent::UsageChanged(*snapshot);
    drop(snapshot);
    let _ = event_tx.send(event).await;
}
