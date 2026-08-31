use omini_config::Settings;
use omini_config::project::ProjectDir;
use omini_domain::config::ThinkingEffort;
use omini_domain::events::{ActiveProfile, ThreadUsageSnapshot};
use omini_provider_api::LlmClient;
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::capabilities::CapabilityStore;

pub async fn toggle_active_profile(
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

pub async fn reject_request(event_tx: &mpsc::Sender<RuntimeToServerEvent>) {
    let _ = event_tx
        .send(RuntimeToServerEvent::error(
            "Cannot handle this request while a run is active".to_string(),
        ))
        .await;
}

pub fn rebuild_system_prompt(
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

pub fn current_context_window(settings: &Settings) -> Option<u32> {
    Some(settings.active_model().context_window)
}

pub struct ModelSelection<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub thinking_effort: Option<ThinkingEffort>,
}

pub struct RuntimeSinks<'a> {
    pub event_tx: &'a mpsc::Sender<RuntimeToServerEvent>,
    pub persistence_tx: &'a mpsc::Sender<RuntimePersistenceEvent>,
    pub usage_state: &'a Arc<Mutex<ThreadUsageSnapshot>>,
}

pub async fn apply_model_selection(
    settings: &mut Settings,
    llm_client: &mut LlmClient,
    project: &ProjectDir,
    thread_id: Option<&str>,
    selection: ModelSelection<'_>,
    sinks: RuntimeSinks<'_>,
) {
    let requested = omini_config::ModelSelection {
        active_provider: selection.provider.to_string(),
        model: selection.model.to_string(),
        thinking_effort: selection.thinking_effort,
    };
    if let Err(error) = settings.select_model(requested) {
        let _ = sinks
            .event_tx
            .send(RuntimeToServerEvent::error(error.to_string()))
            .await;
        return;
    }

    let model = settings.active_model();
    *llm_client = LlmClient::new(
        model.protocol,
        model
            .api_key
            .as_ref()
            .map(|secret| secret.expose().to_string())
            .unwrap_or_default(),
        model.base_url.clone(),
    );

    if let Some(thread_id) = thread_id {
        let _ = sinks
            .persistence_tx
            .send(RuntimePersistenceEvent::UpdateThreadConfig {
                thread_id: thread_id.to_string(),
                provider: model.provider_id.clone(),
                model: model.model_id.clone(),
                thinking_effort: model.thinking_effort.map(|effort| effort.to_string()),
            })
            .await;
    } else if let Ok(mut state) = project.load_state() {
        state.default_provider = Some(model.provider_id.clone());
        state.default_model = Some(model.model_id.clone());
        state.thinking_effort = model.thinking_effort;
        let _ = project.save_state(&state);
    }

    let _ = sinks
        .event_tx
        .send(RuntimeToServerEvent::ModelChanged {
            provider: model.provider_id.clone(),
            model: model.model_id.clone(),
            thinking_effort: model.thinking_effort,
            context_window: Some(model.context_window),
        })
        .await;
    send_usage_snapshot(sinks.event_tx, sinks.usage_state, settings).await;
}

pub async fn apply_thinking_effort(
    settings: &mut Settings,
    project: &ProjectDir,
    thread_id: Option<&str>,
    effort: ThinkingEffort,
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let current = settings.active_model();
    if effort != ThinkingEffort::None && !current.capabilities.thinking {
        let _ = event_tx
            .send(RuntimeToServerEvent::error(format!(
                "当前模型 '{}' 不支持思考模式",
                current.model_id
            )))
            .await;
        return;
    }

    let selection = omini_config::ModelSelection {
        active_provider: current.provider_id.clone(),
        model: current.model_id.clone(),
        thinking_effort: Some(effort),
    };
    let effective_effort = match settings.resolve_model(&selection) {
        Ok(model) => model.thinking_effort,
        Err(error) => {
            let _ = event_tx
                .send(RuntimeToServerEvent::error(error.to_string()))
                .await;
            return;
        }
    };
    if let Some(thread_id) = thread_id {
        if persistence_tx
            .send(RuntimePersistenceEvent::UpdateThreadThinkingEffort {
                thread_id: thread_id.to_string(),
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

    if let Err(error) = settings.select_model(selection) {
        let _ = event_tx
            .send(RuntimeToServerEvent::error(error.to_string()))
            .await;
        return;
    }
    let model = settings.active_model();
    let _ = event_tx
        .send(RuntimeToServerEvent::ModelChanged {
            provider: model.provider_id.clone(),
            model: model.model_id.clone(),
            thinking_effort: model.thinking_effort,
            context_window: Some(model.context_window),
        })
        .await;
}

async fn send_usage_snapshot(
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
    usage_state: &Arc<Mutex<ThreadUsageSnapshot>>,
    settings: &Settings,
) {
    let context_window = current_context_window(settings);
    let event = {
        let mut snapshot = usage_state.lock().expect("thread usage lock poisoned");
        snapshot.context_window = context_window;
        RuntimeToServerEvent::UsageChanged(*snapshot)
    };
    let _ = event_tx.send(event).await;
}
