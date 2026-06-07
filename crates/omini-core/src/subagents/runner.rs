use crate::api::{FinishReason, LlmClient};
use crate::engine::{QueryContext, QueryEngine};
use crate::persistence::SessionRecord;
use crate::skills::SkillSummary;
use crate::subagents::AgentSpec;
use crate::tools::{
    ToolExecutionContext, ToolResult, ToolRuntimeContext, create_subagent_registry_from_parent,
};
use crate::types::config::{ProviderProfile, Settings};
use crate::types::events::{
    EngineToRuntimeEvent, SubagentFinishedEvent, SubagentMessageEvent, SubagentStartedEvent,
    SubagentStatus, SubagentToolResultEvent, SubagentToolUseEvent,
};
use crate::types::message::{ContentBlock, Message, Role};
use chrono::Utc;
use omini_domain::project::sanitize_project_path as sanitize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SubagentRunRequest {
    pub name: String,
    pub prompt: String,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct RuntimeSubagentRunner;

impl RuntimeSubagentRunner {
    pub async fn run_subagent(
        &self,
        request: SubagentRunRequest,
        ctx: ToolExecutionContext,
        runtime: Arc<ToolRuntimeContext>,
    ) -> ToolResult {
        let Some(parent_settings) = ctx.settings.clone() else {
            return ToolResult::error("subagent requires query settings");
        };
        let name = request.name.trim();
        let Some(spec) = runtime.subagent_registry.get(name).cloned() else {
            let available = runtime.subagent_registry.sorted_names();
            let mut msg = format!(
                "unknown subagent '{name}'. Available subagents: {}",
                available.join(", ")
            );
            if !runtime.subagent_registry.diagnostics.is_empty() {
                msg.push_str("\n\nSubagent load warnings:");
                for diagnostic in &runtime.subagent_registry.diagnostics {
                    msg.push_str("\n- ");
                    msg.push_str(diagnostic.message());
                }
            }
            return ToolResult::error(msg);
        };

        run_subagent(spec, request, ctx, runtime, parent_settings).await
    }
}

async fn run_subagent(
    spec: AgentSpec,
    request: SubagentRunRequest,
    ctx: ToolExecutionContext,
    runtime: Arc<ToolRuntimeContext>,
    parent_settings: Arc<Settings>,
) -> ToolResult {
    let Some(parent_tool_registry) = ctx.tool_registry.clone() else {
        return ToolResult::error("subagent requires parent tool registry");
    };
    let (tool_registry, mut warnings) = match create_subagent_registry_from_parent(
        &parent_tool_registry,
        spec.tool_policy.allow.as_deref(),
        spec.tool_policy.deny.as_deref().unwrap_or(&[]),
    ) {
        Ok(result) => result,
        Err(e) => return ToolResult::error(e),
    };

    let (settings, model_warnings) = resolve_subagent_settings(&parent_settings, &spec);
    warnings.extend(model_warnings);

    let session_id = Uuid::new_v4().to_string();
    let subagent_span = tracing::debug_span!(
        "subagent_run",
        parent_session_id = %runtime.session_id,
        subagent_session_id = %session_id,
        run_id = ?runtime.run_id.as_deref(),
        tool_use_id = %ctx.tool_use_id,
        agent_label = %spec.name,
        title = %request.title
    );
    tracing::debug!(
        parent_session_id = %runtime.session_id,
        subagent_session_id = %session_id,
        tool_use_id = %ctx.tool_use_id,
        agent_label = %spec.name,
        title = %request.title,
        "subagent session creating"
    );
    let session_dir = match runtime.session_dir.create_subagent(&session_id) {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!(
                subagent_session_id = %session_id,
                error = %e,
                "failed to create subagent session"
            );
            return ToolResult::error(format!("failed to create subagent session: {e}"));
        }
    };

    let now = Utc::now();
    let session = SessionRecord {
        id: session_id.clone(),
        project_path: sanitize(&settings.cwd),
        parent_session_id: Some(runtime.session_id.clone()),
        spawn_tool_use_id: Some(ctx.tool_use_id.clone()),
        session_type: "subagent".to_string(),
        agent_label: Some(spec.name.clone()),
        provider: settings.active_provider.clone(),
        model: settings.model.clone(),
        thinking_effort: settings.thinking_effort.map(|e| e.to_string()),
        title: Some(request.title.clone()),
        current_context_tokens: 0,
        total_tokens: 0,
        total_cached_tokens: 0,
        created_at: now,
        updated_at: now,
    };
    let _ = ctx
        .event_tx
        .send(EngineToRuntimeEvent::SubagentSessionCreated(session))
        .await;

    let _ = ctx
        .event_tx
        .send(EngineToRuntimeEvent::SubagentStarted(
            SubagentStartedEvent {
                session_id: session_id.clone(),
                parent_session_id: runtime.session_id.clone(),
                spawn_tool_use_id: ctx.tool_use_id.clone(),
                agent_label: spec.name.clone(),
            },
        ))
        .await;

    for warning in &warnings {
        let _ = ctx
            .event_tx
            .send(EngineToRuntimeEvent::Warning(format!(
                "Subagent '{}': {warning}",
                spec.name
            )))
            .await;
    }

    let skill_summaries = subagent_skill_summaries(&tool_registry, &runtime.skill_registry);

    let tool_registry = Arc::new(tool_registry);
    let mut settings = settings;
    settings.system_prompt = Some(subagent_system_prompt(
        &parent_settings,
        &spec,
        &skill_summaries,
    ));
    let settings = Arc::new(settings);
    let llm_client = LlmClient::new(
        settings.endpoint,
        settings.api_key.clone(),
        settings.base_url.clone(),
    );
    let child_runtime = Arc::new(ToolRuntimeContext {
        session_id: session_id.clone(),
        run_id: runtime.run_id.clone(),
        session_type: "subagent".to_string(),
        agent_label: Some(spec.name.clone()),
        session_dir: session_dir.clone(),
        subagent_registry: Arc::clone(&runtime.subagent_registry),
        skill_registry: Arc::clone(&runtime.skill_registry),
        subagent_runner: runtime.subagent_runner.clone(),
        project: runtime.project.clone(),
    });

    let mut messages = vec![Message::from_user_text(request.prompt)];
    let _ = ctx
        .event_tx
        .send(EngineToRuntimeEvent::SubagentMessageProduced(
            SubagentMessageEvent {
                session_id: session_id.clone(),
                message: messages[0].clone(),
                persist_llm_history: true,
            },
        ))
        .await;

    let (child_tx, child_rx) = mpsc::channel::<EngineToRuntimeEvent>(256);
    let bridge = spawn_subagent_bridge(
        child_rx,
        ctx.event_tx.clone(),
        session_id.clone(),
        runtime.session_id.clone(),
        spec.name.clone(),
    );

    let engine = QueryEngine::with_shared_tool_controls(
        ctx.pending_tool_pauses.clone(),
        ctx.permission_engine.clone(),
        ctx.cancel_notify.clone(),
    );
    let result = engine
        .run_query(
            QueryContext {
                messages: &mut messages,
                settings: Arc::clone(&settings),
                llm_client,
                tool_registry,
                active_profile: ctx.active_profile,
                runtime_context: Some(child_runtime),
            },
            child_tx,
            ctx.cancelled.clone(),
        )
        .instrument(subagent_span)
        .await;
    let _ = bridge.await;

    let status = if ctx.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        SubagentStatus::Cancelled
    } else {
        match &result.finish_reason {
            FinishReason::Error(_) => SubagentStatus::Failed,
            _ => SubagentStatus::Completed,
        }
    };
    let _ = ctx
        .event_tx
        .send(EngineToRuntimeEvent::SubagentFinished(
            SubagentFinishedEvent {
                session_id: session_id.clone(),
                status,
            },
        ))
        .await;
    tracing::debug!(
        subagent_session_id = %session_id,
        status = ?status,
        "subagent run finished"
    );

    let summary = extract_final_text(&messages);
    let payload = json!({
        "session_id": session_id,
        "agent_label": spec.name,
        "status": match status {
            SubagentStatus::Running => "running",
            SubagentStatus::Completed => "completed",
            SubagentStatus::Failed => "failed",
            SubagentStatus::Cancelled => "cancelled",
        },
        "summary": summary,
        "warnings": warnings,
        "error": match result.finish_reason {
            FinishReason::Error(e) => Some(e),
            _ => None,
        },
    });

    if matches!(status, SubagentStatus::Failed | SubagentStatus::Cancelled) {
        ToolResult::error(payload.to_string())
    } else {
        ToolResult::ok(payload.to_string())
    }
}

fn resolve_subagent_settings(
    parent_settings: &Settings,
    spec: &AgentSpec,
) -> (Settings, Vec<String>) {
    let mut settings = parent_settings.clone();
    let mut warnings = Vec::new();
    let Some(model_spec) = &spec.model else {
        return (settings, warnings);
    };

    let Some(provider) = parent_settings.providers.get(&model_spec.provider) else {
        warnings.push(format!(
            "provider '{}' is not configured; falling back to {}/{}",
            model_spec.provider, parent_settings.active_provider, parent_settings.model
        ));
        return (settings, warnings);
    };
    if !provider
        .models
        .iter()
        .any(|model| model.id == model_spec.model)
    {
        warnings.push(format!(
            "model '{}' is not configured for provider '{}'; falling back to {}/{}",
            model_spec.model,
            model_spec.provider,
            parent_settings.active_provider,
            parent_settings.model
        ));
        return (settings, warnings);
    }

    apply_provider(
        &mut settings,
        &model_spec.provider,
        &model_spec.model,
        provider,
    );
    (settings, warnings)
}

fn apply_provider(
    settings: &mut Settings,
    provider_name: &str,
    model_name: &str,
    provider: &ProviderProfile,
) {
    settings.active_provider = provider_name.to_string();
    settings.model = model_name.to_string();
    settings.endpoint = provider.endpoint;
    settings.api_key = provider.api_key.clone();
    settings.base_url = provider.base_url.clone();
}

fn spawn_subagent_bridge(
    mut child_rx: mpsc::Receiver<EngineToRuntimeEvent>,
    parent_tx: mpsc::Sender<EngineToRuntimeEvent>,
    session_id: String,
    parent_session_id: String,
    agent_label: String,
) -> tokio::task::JoinHandle<()> {
    let span_parent_session_id = parent_session_id.clone();
    let span_session_id = session_id.clone();
    let span_agent_label = agent_label.clone();
    tokio::spawn(
        async move {
            tracing::debug!("subagent bridge started");
            while let Some(event) = child_rx.recv().await {
                match event {
                    EngineToRuntimeEvent::UserMessageProduced { message, .. } => {
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::SubagentMessageProduced(
                                SubagentMessageEvent {
                                    session_id: session_id.clone(),
                                    message,
                                    persist_llm_history: true,
                                },
                            ))
                            .await;
                    }
                    EngineToRuntimeEvent::MessageProduced(msg) => {
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::SubagentMessageProduced(
                                SubagentMessageEvent {
                                    session_id: session_id.clone(),
                                    message: msg,
                                    persist_llm_history: true,
                                },
                            ))
                            .await;
                    }
                    EngineToRuntimeEvent::ToolResultsProduced(msg) => {
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::SubagentMessageProduced(
                                SubagentMessageEvent {
                                    session_id: session_id.clone(),
                                    message: msg,
                                    persist_llm_history: true,
                                },
                            ))
                            .await;
                    }
                    EngineToRuntimeEvent::ToolResultsDisplayProduced(msg) => {
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::SubagentMessageProduced(
                                SubagentMessageEvent {
                                    session_id: session_id.clone(),
                                    message: msg,
                                    persist_llm_history: false,
                                },
                            ))
                            .await;
                    }
                    EngineToRuntimeEvent::ToolUse(tool_use) => {
                        tracing::debug!(
                            subagent_session_id = %session_id,
                            tool_use_id = %tool_use.id,
                            tool_name = %tool_use.name,
                            "bridging subagent tool use"
                        );
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::SubagentToolUse(
                                SubagentToolUseEvent {
                                    session_id: session_id.clone(),
                                    tool_use,
                                },
                            ))
                            .await;
                    }
                    EngineToRuntimeEvent::ToolResult(tool_result) => {
                        tracing::debug!(
                            subagent_session_id = %session_id,
                            tool_use_id = %tool_result.tool_use_id,
                            is_error = tool_result.is_error,
                            "bridging subagent tool result"
                        );
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::SubagentToolResult(
                                SubagentToolResultEvent {
                                    session_id: session_id.clone(),
                                    tool_result,
                                },
                            ))
                            .await;
                    }
                    EngineToRuntimeEvent::ToolPauseRequested(req) => {
                        tracing::debug!(
                            subagent_session_id = %session_id,
                            tool_use_id = %req.tool_use_id,
                            tool_name = %req.tool_name,
                            "bridging subagent tool pause"
                        );
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::ToolPauseRequested(req))
                            .await;
                    }
                    EngineToRuntimeEvent::PlanSubmitted(plan) => {
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::PlanSubmitted(plan))
                            .await;
                    }
                    EngineToRuntimeEvent::UsageRecorded(usage) => {
                        let _ = parent_tx
                            .send(EngineToRuntimeEvent::SubagentUsageRecorded {
                                session_id: session_id.clone(),
                                usage,
                            })
                            .await;
                    }
                    EngineToRuntimeEvent::Error(e) => {
                        let _ = parent_tx.send(EngineToRuntimeEvent::Error(e)).await;
                    }
                    EngineToRuntimeEvent::Warning(warning) => {
                        let _ = parent_tx.send(EngineToRuntimeEvent::Warning(warning)).await;
                    }
                    EngineToRuntimeEvent::TurnStarted
                    | EngineToRuntimeEvent::LlmHistoryProduced(_)
                    | EngineToRuntimeEvent::TurnEnded
                    | EngineToRuntimeEvent::ThinkingDelta(_)
                    | EngineToRuntimeEvent::TextDelta(_)
                    | EngineToRuntimeEvent::CompactShrinkStarted(_)
                    | EngineToRuntimeEvent::CompactShrinkFinished(_)
                    | EngineToRuntimeEvent::CompactShrinkFailed(_)
                    | EngineToRuntimeEvent::CompactSummaryStarted(_)
                    | EngineToRuntimeEvent::CompactSummaryDelta(_)
                    | EngineToRuntimeEvent::CompactSummaryFinished(_)
                    | EngineToRuntimeEvent::CompactSummaryFailed(_)
                    | EngineToRuntimeEvent::CompactSummaryUsageRecorded(_)
                    | EngineToRuntimeEvent::SubagentStarted(_)
                    | EngineToRuntimeEvent::SubagentSessionCreated(_)
                    | EngineToRuntimeEvent::SubagentUsageRecorded { .. }
                    | EngineToRuntimeEvent::SubagentMessageProduced(_)
                    | EngineToRuntimeEvent::SubagentToolUse(_)
                    | EngineToRuntimeEvent::SubagentToolResult(_)
                    | EngineToRuntimeEvent::SubagentFinished(_) => {}
                }
            }
            tracing::debug!("subagent bridge stopped");
        }
        .instrument(tracing::debug_span!(
            "subagent_bridge",
            parent_session_id = %span_parent_session_id,
            subagent_session_id = %span_session_id,
            agent_label = %span_agent_label
        )),
    )
}

fn subagent_system_prompt(
    parent: &crate::types::config::Settings,
    spec: &AgentSpec,
    skills: &[SkillSummary],
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are running as an isolated subagent for Omini.\n\n");
    if let Some(section) = crate::prompts::language_preference_section(parent) {
        prompt.push_str(&section);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&crate::prompts::project_context_prompt(&parent.cwd));
    if let Some(section) = crate::prompts::skill_section(skills) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    prompt.push_str("\n\n<subagent_instructions>\n");
    prompt.push_str(
        "Return a concise final result for the parent agent. Do not try to spawn subagents.\n\n",
    );
    prompt.push_str("Agent name: ");
    prompt.push_str(&spec.name);
    prompt.push_str("\nDescription: ");
    prompt.push_str(&spec.description);
    prompt.push_str("\n\n");
    prompt.push_str(&spec.instructions);
    prompt.push_str("\n</subagent_instructions>");
    prompt
}

fn subagent_skill_summaries(
    tool_registry: &crate::tools::ToolRegistry,
    skill_registry: &crate::skills::SkillRegistry,
) -> Vec<SkillSummary> {
    if tool_registry.contains("skill") {
        skill_registry.injected_summaries()
    } else {
        Vec::new()
    }
}

fn extract_final_text(messages: &[Message]) -> String {
    messages
        .last()
        .filter(|msg| msg.role == Role::Assistant)
        .map(|msg| {
            msg.content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.trim()),
                    _ => None,
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "(no final text)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagents::{AgentModelSpec, AgentSource, AgentToolPolicy};
    use crate::types::config::{ModelConfig, ProviderProfile, ProviderType, Settings};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_spec(model: Option<AgentModelSpec>) -> AgentSpec {
        AgentSpec {
            name: "test".to_string(),
            description: "Test agent".to_string(),
            instructions: "Do the task.".to_string(),
            tool_policy: AgentToolPolicy::default(),
            model,
            source: AgentSource::BuiltIn,
        }
    }

    fn test_settings() -> Settings {
        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderProfile {
                name: "OpenAI".to_string(),
                endpoint: ProviderType::OpenAI,
                api_key: "openai-key".to_string(),
                base_url: "https://openai.example".to_string(),
                models: vec![ModelConfig {
                    id: "gpt-5.4".to_string(),
                    name: None,
                    limit: 256000,
                    thinking: true,
                    input_modalities: None,
                }],
            },
        );
        providers.insert(
            "anthropic".to_string(),
            ProviderProfile {
                name: "Anthropic".to_string(),
                endpoint: ProviderType::Anthropic,
                api_key: "anthropic-key".to_string(),
                base_url: "https://anthropic.example".to_string(),
                models: vec![ModelConfig {
                    id: "claude-test".to_string(),
                    name: None,
                    limit: 200000,
                    thinking: false,
                    input_modalities: None,
                }],
            },
        );

        Settings {
            api_key: "openai-key".to_string(),
            base_url: "https://openai.example".to_string(),
            model: "gpt-5.4".to_string(),
            endpoint: ProviderType::OpenAI,
            providers,
            active_provider: "openai".to_string(),
            system_prompt: None,
            language: None,
            max_turns: None,
            cwd: PathBuf::from("/tmp"),
            thinking_effort: None,
            permissions: None,
            compact: Default::default(),
            mcp_servers: HashMap::new(),
        }
    }

    #[test]
    fn extract_final_text_uses_only_last_assistant_message() {
        let messages = vec![
            Message::new(
                Role::Assistant,
                vec![ContentBlock::from_text("older answer".to_string())],
            ),
            Message::from_user_text("tool result".to_string()),
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_thinking("hidden reasoning".to_string()),
                    ContentBlock::from_text("final answer".to_string()),
                ],
            ),
        ];

        assert_eq!(extract_final_text(&messages), "final answer");
    }

    #[test]
    fn extract_final_text_does_not_fall_back_to_older_assistant_message() {
        let messages = vec![
            Message::new(
                Role::Assistant,
                vec![ContentBlock::from_text("older answer".to_string())],
            ),
            Message::from_user_text("latest non-assistant message".to_string()),
        ];

        assert_eq!(extract_final_text(&messages), "(no final text)");
    }

    #[test]
    fn extract_final_text_uses_partial_interrupted_assistant_message() {
        let messages = vec![
            Message::from_user_text("explore the repository".to_string()),
            Message::new(
                Role::Assistant,
                vec![ContentBlock::from_text(
                    "partial findings before stream interruption".to_string(),
                )],
            ),
        ];

        assert_eq!(
            extract_final_text(&messages),
            "partial findings before stream interruption"
        );
    }

    #[test]
    fn resolve_subagent_settings_switches_to_configured_model() {
        let parent = test_settings();
        let spec = test_spec(Some(AgentModelSpec {
            provider: "anthropic".to_string(),
            model: "claude-test".to_string(),
        }));

        let (settings, warnings) = resolve_subagent_settings(&parent, &spec);

        assert!(warnings.is_empty());
        assert_eq!(settings.active_provider, "anthropic");
        assert_eq!(settings.model, "claude-test");
        assert_eq!(settings.endpoint, ProviderType::Anthropic);
        assert_eq!(settings.api_key, "anthropic-key");
    }

    #[test]
    fn resolve_subagent_settings_falls_back_when_model_missing() {
        let parent = test_settings();
        let spec = test_spec(Some(AgentModelSpec {
            provider: "anthropic".to_string(),
            model: "missing".to_string(),
        }));

        let (settings, warnings) = resolve_subagent_settings(&parent, &spec);

        assert_eq!(settings.active_provider, "openai");
        assert_eq!(settings.model, "gpt-5.4");
        assert_eq!(settings.endpoint, ProviderType::OpenAI);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("falling back"));
    }

    #[test]
    fn subagent_system_prompt_includes_parent_language_preference() {
        let mut parent = test_settings();
        parent.language = Some("  en  ".to_string());
        let spec = test_spec(None);

        let prompt = subagent_system_prompt(&parent, &spec, &[]);

        assert!(prompt.contains("<language_preference>"));
        assert!(prompt.contains("`en`"));
        assert!(prompt.contains("<subagent_instructions>"));
    }

    #[test]
    fn subagent_system_prompt_includes_skill_summaries_when_provided() {
        let parent = test_settings();
        let spec = test_spec(None);
        let skills = vec![SkillSummary {
            name: "commit-message".to_string(),
            description: "Suggest commit messages".to_string(),
            directory: PathBuf::from("/tmp/skill"),
        }];

        let prompt = subagent_system_prompt(&parent, &spec, &skills);

        assert!(prompt.contains("<skill_instructions>"));
        assert!(prompt.contains("- `commit-message`: Suggest commit messages"));
        assert!(prompt.contains("Use the `skill` tool"));
        assert!(prompt.contains("<subagent_instructions>"));
        assert!(!prompt.contains("/tmp/skill"));
    }

    #[test]
    fn subagent_system_prompt_omits_skill_section_when_no_summaries_provided() {
        let parent = test_settings();
        let spec = test_spec(None);

        let prompt = subagent_system_prompt(&parent, &spec, &[]);

        assert!(!prompt.contains("<skill_instructions>"));
        assert!(prompt.contains("<subagent_instructions>"));
    }

    #[test]
    fn subagent_skill_summaries_follow_skill_tool_availability() {
        let parent = test_settings();
        let skill_registry = crate::skills::load_skill_registry(&parent.cwd);
        let with_skill = crate::tools::create_subagent_registry(&["skill".to_string()]);
        let without_skill = crate::tools::create_subagent_registry(&["read".to_string()]);

        let visible = subagent_skill_summaries(&with_skill, &skill_registry);
        let hidden = subagent_skill_summaries(&without_skill, &skill_registry);

        assert!(visible.iter().any(|skill| skill.name == "commit-message"));
        assert!(hidden.is_empty());
    }
}
