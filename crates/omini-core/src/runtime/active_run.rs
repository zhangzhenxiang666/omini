use crate::error::CoreError;
use crate::execution::host::AgentHost;
use omini_config::Settings;
use omini_config::project::ProjectDir;
use omini_domain::config::ThinkingEffort;
use omini_provider_api::LlmClient;
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::thread_domain::{ActiveProfile, ThreadUsageSnapshot};
use std::sync::{Arc, Mutex};

use super::capabilities::CapabilityStore;
use crate::execution::handle::OutputHandle;

pub async fn toggle_active_profile(
    active_profile: &mut ActiveProfile,
    settings: &mut Settings,
    capabilities: &CapabilityStore,
    output: &OutputHandle,
) {
    let next = match *active_profile {
        ActiveProfile::Main => ActiveProfile::Auto,
        ActiveProfile::Auto => ActiveProfile::Main,
        ActiveProfile::Plan => return,
    };
    *active_profile = next;
    rebuild_system_prompt(settings, capabilities, next);
    let _ = output
        .send_event(RuntimeToServerEvent::ActiveProfileChanged(next))
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
    pub output: &'a OutputHandle,
    pub host: &'a dyn AgentHost,
    pub usage_state: &'a Arc<Mutex<ThreadUsageSnapshot>>,
}

/// 应用模型切换：先校验并持久化线程配置，成功后才改运行时配置。
///
/// 失败返回错误且有效配置不变（确认语义：已应用且必要持久化完成）。
pub async fn apply_model_selection(
    settings: &mut Settings,
    llm_client: &mut LlmClient,
    project: &ProjectDir,
    thread_id: Option<&str>,
    selection: ModelSelection<'_>,
    sinks: RuntimeSinks<'_>,
) -> Result<(), CoreError> {
    let requested = omini_config::ModelSelection {
        active_provider: selection.provider.to_string(),
        model: selection.model.to_string(),
        thinking_effort: selection.thinking_effort,
    };
    // 在副本上先完成校验，避免半应用状态。
    let mut candidate = settings.clone();
    candidate
        .select_model(requested)
        .map_err(|error| CoreError::invalid_model_selection(error.to_string()))?;
    // 取 owned 快照，避免后续把 candidate 移回 settings 时悬挂借用。
    let model = candidate.active_model().clone();

    if let Some(thread_id) = thread_id {
        sinks
            .host
            .update_thread_config(
                thread_id,
                &model.provider_id,
                &model.model_id,
                model
                    .thinking_effort
                    .map(|effort| effort.to_string())
                    .as_deref(),
            )
            .await
            .map_err(CoreError::from)?;
    } else {
        // 非线程场景（项目默认）沿用文件状态；失败不应用。
        let mut state = project
            .load_state()
            .map_err(|error| CoreError::project_state("读取项目状态失败", error))?;
        state.default_provider = Some(model.provider_id.clone());
        state.default_model = Some(model.model_id.clone());
        state.thinking_effort = model.thinking_effort;
        project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("保存项目状态失败", error))?;
    }

    // 持久化成功后应用运行时配置。
    *settings = candidate;
    *llm_client = LlmClient::new(
        model.protocol,
        model
            .api_key
            .as_ref()
            .map(|secret| secret.expose().to_string())
            .unwrap_or_default(),
        model.base_url.clone(),
    );

    let _ = sinks
        .output
        .send_event(RuntimeToServerEvent::ModelChanged {
            provider: model.provider_id.clone(),
            model: model.model_id.clone(),
            thinking_effort: model.thinking_effort,
            context_window: Some(model.context_window),
        })
        .await;
    send_usage_snapshot(sinks.output, sinks.usage_state, settings).await;
    Ok(())
}

/// 应用思考强度：持久化成功后才改配置；失败返回错误且快照不变。
pub async fn apply_thinking_effort(
    settings: &mut Settings,
    project: &ProjectDir,
    thread_id: Option<&str>,
    effort: ThinkingEffort,
    output: &OutputHandle,
    host: &dyn AgentHost,
) -> Result<(), CoreError> {
    let current = settings.active_model();
    if effort != ThinkingEffort::None && !current.capabilities.thinking {
        return Err(CoreError::invalid_model_selection(format!(
            "当前模型 '{}' 不支持思考模式",
            current.model_id
        )));
    }

    let selection = omini_config::ModelSelection {
        active_provider: current.provider_id.clone(),
        model: current.model_id.clone(),
        thinking_effort: Some(effort),
    };
    let effective_effort = settings
        .resolve_model(&selection)
        .map_err(|error| CoreError::invalid_model_selection(error.to_string()))?
        .thinking_effort;
    if let Some(thread_id) = thread_id {
        host.update_thread_thinking_effort(
            thread_id,
            effective_effort.map(|effort| effort.to_string()).as_deref(),
        )
        .await
        .map_err(CoreError::from)?;
    } else {
        let mut state = project
            .load_state()
            .map_err(|error| CoreError::project_state("读取项目状态失败", error))?;
        state.thinking_effort = effective_effort;
        project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("保存项目状态失败", error))?;
    }

    let mut candidate = settings.clone();
    candidate
        .select_model(selection)
        .map_err(|error| CoreError::invalid_model_selection(error.to_string()))?;
    *settings = candidate;
    let model = settings.active_model();
    let _ = output
        .send_event(RuntimeToServerEvent::ModelChanged {
            provider: model.provider_id.clone(),
            model: model.model_id.clone(),
            thinking_effort: model.thinking_effort,
            context_window: Some(model.context_window),
        })
        .await;
    Ok(())
}

async fn send_usage_snapshot(
    output: &OutputHandle,
    usage_state: &Arc<Mutex<ThreadUsageSnapshot>>,
    settings: &Settings,
) {
    let context_window = current_context_window(settings);
    let event = {
        let mut snapshot = usage_state.lock().expect("thread usage lock poisoned");
        snapshot.context_window = context_window;
        RuntimeToServerEvent::UsageChanged(*snapshot)
    };
    let _ = output.send_event(event).await;
}
