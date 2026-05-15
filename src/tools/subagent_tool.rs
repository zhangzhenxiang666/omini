use super::{Tool, ToolExecutionContext, ToolResult};
use crate::api::{FinishReason, LlmClient};
use crate::config::project::sanitize;
use crate::db::{self, Session};
use crate::engine::{QueryContext, QueryEngine};
use crate::subagents::{AgentSpec, load_agent_registry};
use crate::tools::{ToolRuntimeContext, create_subagent_registry};
use crate::types::events::{
    EngineToRuntimeEvent, SubagentFinishedEvent, SubagentMessageEvent, SubagentStartedEvent,
    SubagentStatus, SubagentToolResultEvent, SubagentToolUseEvent,
};
use crate::types::message::{ContentBlock, Message, Role};
use async_trait::async_trait;
use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SubagentInput {
    /// Subagent name to run.
    pub name: String,
    /// Task prompt for the subagent. The parent agent must describe the concrete task.
    pub prompt: String,
    /// Optional short title shown in the UI for this subagent task.
    pub title: Option<String>,
}

pub struct SubagentTool;

#[async_trait]
impl Tool for SubagentTool {
    type Input = SubagentInput;
    type Prepared = SubagentInput;

    fn name(&self) -> &str {
        "subagent"
    }

    fn description(&self) -> &str {
        concat!(
            "Run an isolated subagent for a focused task and wait for its final result.\n",
            "\n",
            "Input fields:\n",
            "  name    The subagent name to run.\n",
            "  prompt  The concrete task for that subagent. This field is required.\n",
            "  title   Optional short UI title for this subagent task.\n",
            "\n",
            "The subagent has its own session, context, system instructions, ",
            "and tool allowlist. Its intermediate messages are hidden from the main context. ",
            "You may call this tool multiple times in one assistant turn to run subagents in parallel.\n",
            "\n",
            "Built-in subagents: default, explorer, worker. Custom subagents are loaded from ",
            ".omini/agents/*.md in the current workspace."
        )
    }

    async fn prepare(&self, mut input: SubagentInput) -> Result<Self::Prepared, ToolResult> {
        if input.name.trim().is_empty() {
            return Err(ToolResult::error("name must not be empty"));
        }
        if input.prompt.trim().is_empty() {
            return Err(ToolResult::error("prompt must not be empty"));
        }
        input.title = input.title.and_then(|title| {
            let title = title.trim();
            if title.is_empty() {
                None
            } else {
                Some(title.chars().take(80).collect())
            }
        });
        Ok(input)
    }

    async fn execute_prepared(
        &self,
        input: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let Some(runtime) = ctx.runtime.clone() else {
            return ToolResult::error("subagent requires runtime context");
        };
        if runtime.session_type == "subagent" {
            return ToolResult::error("subagent tool is not available inside subagents");
        }

        let registry = load_agent_registry(&runtime.settings_snapshot.cwd);
        let name = input.name.trim();
        let Some(spec) = registry.agents.get(name).cloned() else {
            let mut available: Vec<_> = registry.agents.keys().cloned().collect();
            available.sort();
            let mut msg = format!(
                "unknown subagent '{name}'. Available subagents: {}",
                available.join(", ")
            );
            if !registry.diagnostics.is_empty() {
                msg.push_str("\n\nSubagent load warnings:");
                for diagnostic in registry.diagnostics {
                    msg.push_str("\n- ");
                    msg.push_str(diagnostic.message());
                }
            }
            return ToolResult::error(msg);
        };
        run_subagent(spec, input, ctx, runtime).await
    }
}

async fn run_subagent(
    spec: AgentSpec,
    input: SubagentInput,
    ctx: ToolExecutionContext,
    runtime: Arc<ToolRuntimeContext>,
) -> ToolResult {
    let session_id = Uuid::new_v4().to_string();
    let session_dir = match runtime.session_dir.create_subagent(&session_id) {
        Ok(dir) => dir,
        Err(e) => return ToolResult::error(format!("failed to create subagent session: {e}")),
    };

    let now = Utc::now();
    let model = runtime.settings_snapshot.model.clone();
    let thinking_effort = runtime.settings_snapshot.thinking_effort;
    let title = input.title.clone();
    let session = Session {
        id: session_id.clone(),
        project_path: sanitize(&runtime.settings_snapshot.cwd),
        parent_session_id: Some(runtime.session_id.clone()),
        spawn_tool_use_id: Some(ctx.tool_use_id.clone()),
        session_type: "subagent".to_string(),
        agent_label: Some(spec.name.clone()),
        provider: runtime.settings_snapshot.active_provider.clone(),
        model: model.clone(),
        thinking_effort: thinking_effort.map(|e| e.to_string()),
        title,
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
    let mut settings = (*runtime.settings_snapshot).clone();
    settings.model = model;
    settings.thinking_effort = thinking_effort;
    settings.system_prompt = Some(subagent_system_prompt(&runtime.settings_snapshot, &spec));
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
        settings_snapshot: Arc::clone(&settings),
        project: runtime.project.clone(),
    });

    let mut messages = vec![Message::from_user_text(input.prompt)];
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
        ctx.permission_policy.clone(),
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
}
