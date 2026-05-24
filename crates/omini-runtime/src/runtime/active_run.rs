use crate::api::LlmClient;
use crate::command;
use crate::command::CommandRegistry;
use crate::config::project::ProjectDir;
use crate::db;
use crate::types::config::Settings;
use crate::types::config::ThinkingEffort;
use crate::types::events::{
    ActiveProfile, InteractionRequest, RuntimeToUiEvent, SessionUsageSnapshot,
};
use tokio::sync::mpsc;

use super::service::CapabilityStore;
use super::service::PendingInteraction;

pub(super) async fn handle_command(
    text: &str,
    pending_interaction: &mut Option<PendingInteraction>,
    command_registry: &CommandRegistry,
    settings: &mut Settings,
    project: &ProjectDir,
    session_id: Option<&str>,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) {
    let Some(parsed) = command::parse(text) else {
        return;
    };

    match parsed.name {
        "model" => {
            *pending_interaction = Some(PendingInteraction::ModelSelect);
            let _ = event_tx
                .send(RuntimeToUiEvent::InteractionRequest(
                    model_selection_request(settings),
                ))
                .await;
        }
        "effort" => {
            apply_effort_selection(settings, project, session_id, parsed.args, event_tx).await;
        }
        "thinking" => match command::thinking::apply_thinking_display(project, parsed.args) {
            Ok(show) => {
                let _ = event_tx
                    .send(RuntimeToUiEvent::ThinkingDisplayChanged { show })
                    .await;
                let _ = event_tx
                    .send(RuntimeToUiEvent::CommandNotice(
                        command::thinking::thinking_display_notice(show),
                    ))
                    .await;
            }
            Err(error) => {
                let _ = event_tx.send(RuntimeToUiEvent::Error(error)).await;
            }
        },
        "help" | "?" => {
            let _ = event_tx
                .send(RuntimeToUiEvent::ShowHelpDrawer(
                    command_registry.summaries(),
                ))
                .await;
        }
        _ => {
            reject_request(event_tx).await;
        }
    }
}

pub(super) async fn toggle_active_profile(
    active_profile: &mut ActiveProfile,
    settings: &mut Settings,
    capabilities: &CapabilityStore,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) {
    let next = match *active_profile {
        ActiveProfile::Main => ActiveProfile::Auto,
        ActiveProfile::Auto => ActiveProfile::Main,
        ActiveProfile::Plan => return,
    };
    *active_profile = next;
    rebuild_system_prompt(settings, capabilities, next);
    let _ = event_tx
        .send(RuntimeToUiEvent::ActiveProfileChanged(next))
        .await;
}

pub(super) async fn reject_request(event_tx: &mpsc::Sender<RuntimeToUiEvent>) {
    let _ = event_tx
        .send(RuntimeToUiEvent::Error(
            "Cannot handle this request while a run is active".to_string(),
        ))
        .await;
}

fn model_selection_request(settings: &Settings) -> InteractionRequest {
    InteractionRequest::ModelSelection {
        providers: settings.providers.clone(),
        current_provider: settings.active_provider.clone(),
        current_model: settings.model.clone(),
    }
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

pub(super) async fn apply_model_selection(
    settings: &mut Settings,
    llm_client: &mut LlmClient,
    project: &ProjectDir,
    session_id: Option<&str>,
    selection: ModelSelection<'_>,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) {
    let provider = selection.provider;
    let model = selection.model;
    let thinking_effort = selection.thinking_effort;
    if let Some(profile) = settings.providers.get(provider) {
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
            let _ = db::global_db()
                .update_session_config(sid, provider, model, te.as_deref())
                .await;
        } else if let Ok(mut state) = project.load_state() {
            state.default_provider = Some(provider.to_string());
            state.default_model = Some(model.to_string());
            state.thinking_effort = thinking_effort;
            let _ = project.save_state(&state);
        }

        let _ = event_tx
            .send(RuntimeToUiEvent::ModelChanged {
                provider: provider.to_string(),
                model: model.to_string(),
                thinking_effort: settings.thinking_effort,
                context_window: current_context_window(settings),
            })
            .await;
        send_usage_snapshot(event_tx, session_id, settings).await;
    } else {
        let _ = event_tx
            .send(RuntimeToUiEvent::Error(format!(
                "提供商 '{provider}' 不存在"
            )))
            .await;
    }
}

async fn apply_effort_selection(
    settings: &mut Settings,
    project: &ProjectDir,
    session_id: Option<&str>,
    args: &str,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) {
    let mut parts = args.split_whitespace();
    let Some(value) = parts.next() else {
        let _ = event_tx
            .send(RuntimeToUiEvent::Error(
                "请提供思考程度，用法: /effort none | low | medium | high".to_string(),
            ))
            .await;
        return;
    };
    if parts.next().is_some() {
        let _ = event_tx
            .send(RuntimeToUiEvent::Error(
                "参数过多，用法: /effort none | low | medium | high".to_string(),
            ))
            .await;
        return;
    }

    let effort = match value.parse::<ThinkingEffort>() {
        Ok(effort) => effort,
        Err(()) => {
            let _ = event_tx
                .send(RuntimeToUiEvent::Error(format!(
                    "无效的思考程度 '{value}'，可用值: none | low | medium | high"
                )))
                .await;
            return;
        }
    };
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
                .send(RuntimeToUiEvent::Error(format!(
                    "当前模型 '{}' 不支持思考模式",
                    settings.model
                )))
                .await;
            return;
        }
    }

    let stored_effort = effort.to_string();
    if let Some(sid) = session_id {
        if let Err(e) = db::global_db()
            .update_session_thinking_effort(sid, Some(&stored_effort))
            .await
        {
            let _ = event_tx
                .send(RuntimeToUiEvent::Error(format!("更新思考程度失败: {e}")))
                .await;
            return;
        }
    } else {
        let mut state = match project.load_state() {
            Ok(state) => state,
            Err(e) => {
                let _ = event_tx
                    .send(RuntimeToUiEvent::Error(format!("读取项目状态失败: {e}")))
                    .await;
                return;
            }
        };
        state.thinking_effort = Some(effort);
        if let Err(e) = project.save_state(&state) {
            let _ = event_tx
                .send(RuntimeToUiEvent::Error(format!("保存项目状态失败: {e}")))
                .await;
            return;
        }
    }

    settings.thinking_effort = Some(effort);
    let _ = event_tx
        .send(RuntimeToUiEvent::ModelChanged {
            provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort,
            context_window: current_context_window(settings),
        })
        .await;
}

async fn send_usage_snapshot(
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    session_id: Option<&str>,
    settings: &Settings,
) {
    let context_window = current_context_window(settings);
    let event = if let Some(session_id) = session_id {
        match db::global_db().get_session(session_id).await {
            Ok(Some(session)) => RuntimeToUiEvent::UsageChanged(usage_snapshot_from_session(
                &session,
                context_window,
            )),
            _ => return,
        }
    } else {
        RuntimeToUiEvent::UsageChanged(SessionUsageSnapshot {
            context_window,
            ..SessionUsageSnapshot::default()
        })
    };
    let _ = event_tx.send(event).await;
}

fn usage_snapshot_from_session(
    session: &crate::db::Session,
    context_window: Option<u32>,
) -> SessionUsageSnapshot {
    SessionUsageSnapshot {
        current_context_tokens: session.current_context_tokens,
        total_tokens: session.total_tokens,
        total_cached_tokens: session.total_cached_tokens,
        context_window,
    }
}
