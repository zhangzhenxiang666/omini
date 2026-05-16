use crate::api::{FinishReason, LlmClient};
use crate::config::project::sanitize;
use crate::db::{self, Session};
use crate::engine::{QueryContext, QueryEngine};
use crate::subagents::AgentSpec;
use crate::tools::{
    ToolExecutionContext, ToolResult, ToolRuntimeContext, create_subagent_registry,
};
use crate::types::config::Settings;
use crate::types::events::{
    EngineToRuntimeEvent, SubagentFinishedEvent, SubagentMessageEvent, SubagentStartedEvent,
    SubagentStatus, SubagentToolResultEvent, SubagentToolUseEvent,
};
use crate::types::message::{ContentBlock, Message, Role};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SubagentRunRequest {
    pub name: String,
    pub prompt: String,
    pub title: Option<String>,
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
    let session_id = Uuid::new_v4().to_string();
    let session_dir = match runtime.session_dir.create_subagent(&session_id) {
        Ok(dir) => dir,
        Err(e) => return ToolResult::error(format!("failed to create subagent session: {e}")),
    };

    let now = Utc::now();
    let model = parent_settings.model.clone();
    let thinking_effort = parent_settings.thinking_effort;
    let session = Session {
        id: session_id.clone(),
        project_path: sanitize(&parent_settings.cwd),
        parent_session_id: Some(runtime.session_id.clone()),
        spawn_tool_use_id: Some(ctx.tool_use_id.clone()),
        session_type: "subagent".to_string(),
        agent_label: Some(spec.name.clone()),
        provider: parent_settings.active_provider.clone(),
        model: model.clone(),
        thinking_effort: thinking_effort.map(|e| e.to_string()),
        title: request.title.clone(),
        message_count: 0,
        created_at: now,
        updated_at: now,
    };
    if let Err(e) = db::global_db().create_session(&session).await {
        return ToolResult::error(format!("failed to persist subagent session: {e}"));
    }

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

    let tool_registry = Arc::new(create_subagent_registry(&spec.allowed_tools));
    let mut settings = (*parent_settings).clone();
    settings.model = model;
    settings.thinking_effort = thinking_effort;
    settings.system_prompt = Some(subagent_system_prompt(&parent_settings, &spec));
    let settings = Arc::new(settings);
    let llm_client = LlmClient::new(
        settings.endpoint,
        settings.api_key.clone(),
        settings.base_url.clone(),
    );
    let child_runtime = Arc::new(ToolRuntimeContext {
        session_id: session_id.clone(),
        session_type: "subagent".to_string(),
        agent_label: Some(spec.name.clone()),
        session_dir: session_dir.clone(),
        subagent_registry: Arc::clone(&runtime.subagent_registry),
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
            },
        ))
        .await;

    let (child_tx, child_rx) = mpsc::channel::<EngineToRuntimeEvent>(256);
    let bridge = spawn_subagent_bridge(child_rx, ctx.event_tx.clone(), session_id.clone());

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
                runtime_context: Some(child_runtime),
            },
            child_tx,
            ctx.cancelled.clone(),
        )
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

    let _ = db::global_db()
        .update_session_msg_count(&session_id, messages.len() as i64)
        .await;

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

fn spawn_subagent_bridge(
    mut child_rx: mpsc::Receiver<EngineToRuntimeEvent>,
    parent_tx: mpsc::Sender<EngineToRuntimeEvent>,
    session_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = child_rx.recv().await {
            match event {
                EngineToRuntimeEvent::UserMessageProduced(msg) => {
                    let _ = parent_tx
                        .send(EngineToRuntimeEvent::SubagentMessageProduced(
                            SubagentMessageEvent {
                                session_id: session_id.clone(),
                                message: msg,
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
                            },
                        ))
                        .await;
                }
                EngineToRuntimeEvent::ToolUse(tool_use) => {
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
                    let _ = parent_tx
                        .send(EngineToRuntimeEvent::ToolPauseRequested(req))
                        .await;
                }
                EngineToRuntimeEvent::Error(e) => {
                    let _ = parent_tx.send(EngineToRuntimeEvent::Error(e)).await;
                }
                EngineToRuntimeEvent::TurnStarted
                | EngineToRuntimeEvent::TurnEnded
                | EngineToRuntimeEvent::ThinkingDelta(_)
                | EngineToRuntimeEvent::TextDelta(_)
                | EngineToRuntimeEvent::SubagentStarted(_)
                | EngineToRuntimeEvent::SubagentMessageProduced(_)
                | EngineToRuntimeEvent::SubagentToolUse(_)
                | EngineToRuntimeEvent::SubagentToolResult(_)
                | EngineToRuntimeEvent::SubagentFinished(_) => {}
            }
        }
    })
}

fn subagent_system_prompt(parent: &crate::types::config::Settings, spec: &AgentSpec) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are running as an isolated subagent for Omini.\n\n");
    prompt.push_str(&crate::prompts::project_context_prompt(&parent.cwd));
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
}
