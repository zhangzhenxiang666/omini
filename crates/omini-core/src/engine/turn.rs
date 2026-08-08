use super::state::{FinalizationReason, REPEAT_LIMIT, RepeatGuard, TurnMode, TurnOutcome};
use super::tool::{
    ToolDrain, ToolExecutor, ToolRunResult, abort_tasks, align_results, commit_results,
    drain_tasks, reconcile_cancelled,
};
use super::{QueryContext, QueryEngine};
use crate::error::RuntimeError;
use crate::prompts::get_max_steps_prompt;
use crate::runtime::compact::{self, AutoCompactState};
use crate::types::events::EngineToRuntimeEvent;
use omini_domain::events::CompactTrigger;
use omini_domain::message::{
    ContentBlock, Message, Role, TextBlock, ThinkingBlock, ToolResultBlock, ToolUseBlock,
};
use omini_domain::tool::ToolDefinition;
use omini_provider_api::{ApiEvent, ApiRequest, FinishReason, StreamError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::StreamExt;
use tracing::Instrument;

const ORPHANED_TOOL_ACTIVITY: &str =
    "LLM stream emitted tool activity but Done did not include an assistant tool_use message";

impl QueryEngine {
    /// 执行一个完整 LLM Turn，但不决定是否开启下一 Turn。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_turn(
        &self,
        ctx: &mut QueryContext<'_>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
        cancelled: &Arc<AtomicBool>,
        tool_definitions: &[ToolDefinition],
        tool_executor: &ToolExecutor,
        tool_tasks: &mut JoinSet<ToolRunResult>,
        repeat_guard: &mut RepeatGuard,
        compact_state: &mut AutoCompactState,
        mode: TurnMode,
        turn_index: usize,
    ) -> TurnOutcome {
        debug_assert!(tool_tasks.is_empty());
        let _ = event_tx.send(EngineToRuntimeEvent::TurnStarted).await;

        let compact_context = compact::CompactRequestContext {
            settings: &ctx.settings,
            llm_client: &ctx.llm_client,
            tool_definitions,
            runtime_context: ctx.runtime_context.as_deref(),
            event_tx,
            trigger: CompactTrigger::Auto,
            custom_instructions: None,
            cancel_token: compact::CompactCancelToken::new(cancelled, &self.cancel_notify),
        };
        let _ = compact::auto_compact_if_needed(ctx.messages, &compact_context, compact_state)
            .instrument(tracing::debug_span!("compact", turn_index))
            .await;

        let request = ApiRequest {
            messages: ctx.messages,
            model: &ctx.settings.model,
            system_prompt: mode
                .is_finalization()
                .then(get_max_steps_prompt)
                .or(ctx.settings.system_prompt.as_deref()),
            tools: (!mode.is_finalization()).then_some(tool_definitions),
            max_tokens: None,
            temperature: None,
            thinking_effort: ctx.settings.thinking_effort,
            extra_headers: ctx
                .settings
                .current_model_config()
                .and_then(|model| model.extra_headers.as_ref()),
            extra_body: ctx
                .settings
                .current_model_config()
                .and_then(|model| model.extra_body.as_ref()),
        };

        let mut stream = match crate::util::cancel::invoke_or_cancel(
            ctx.llm_client.invoke(request),
            cancelled,
            &self.cancel_notify,
        )
        .instrument(tracing::debug_span!("llm_request", turn_index))
        .await
        {
            Some(Ok(stream)) => stream,
            Some(Err(error)) => {
                let error = RuntimeError::ProviderRequest(error).to_string();
                let _ = event_tx
                    .send(EngineToRuntimeEvent::Error(error.clone()))
                    .await;
                let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                return TurnOutcome::Interrupted {
                    finish_reason: FinishReason::Error(error),
                };
            }
            None => {
                let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                return TurnOutcome::Interrupted {
                    finish_reason: FinishReason::Error("Cancelled".to_string()),
                };
            }
        };

        let mut partial_blocks = Vec::new();
        let mut ready_results = Vec::new();
        let mut completion = None;
        let mut stream_error = None;
        let mut stream_cancelled = false;
        let mut requested_finalization = None;

        loop {
            let next = tokio::select! {
                event = stream.next() => event,
                _ = self.cancel_notify.notified() => {
                    if cancelled.load(Ordering::Relaxed) {
                        stream_cancelled = true;
                        break;
                    }
                    continue;
                }
            };

            let Some(event) = next else {
                break;
            };
            if cancelled.load(Ordering::Relaxed) {
                stream_cancelled = true;
                break;
            }

            match event {
                Ok(ApiEvent::Text(delta)) => {
                    push_text_delta(&mut partial_blocks, &delta);
                    let _ = event_tx.send(EngineToRuntimeEvent::TextDelta(delta)).await;
                }
                Ok(ApiEvent::Thinking(delta)) => {
                    push_thinking_delta(&mut partial_blocks, &delta);
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::ThinkingDelta(delta))
                        .await;
                }
                Ok(ApiEvent::ToolUse(tool_use)) => {
                    partial_blocks.push(ContentBlock::ToolUse(tool_use.clone()));
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::ToolUse(tool_use.clone()))
                        .await;

                    if let Some(reason) =
                        rejection_reason(&tool_use, mode, &mut requested_finalization, repeat_guard)
                    {
                        let block = ToolResultBlock {
                            tool_use_id: tool_use.id.clone(),
                            is_error: true,
                            content: reason,
                            metadata: None,
                        };
                        let _ = event_tx
                            .send(EngineToRuntimeEvent::ToolResult(block.clone()))
                            .await;
                        ready_results.push(ToolRunResult::new(block, None));
                    } else {
                        let executor = tool_executor.clone();
                        tool_tasks.spawn(async move { executor.execute(tool_use).await });
                    }
                }
                Ok(ApiEvent::Done(done)) => {
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::UsageRecorded(done.usage))
                        .await;
                    completion = Some(done);
                }
                Err(error) => {
                    stream_error = Some(RuntimeError::ProviderStream(error));
                    break;
                }
            }
        }

        if stream_cancelled || cancelled.load(Ordering::Relaxed) {
            self.finish_interrupted_turn(
                ctx.messages,
                partial_blocks,
                ready_results,
                tool_tasks,
                event_tx,
                None,
            )
            .await;
            return TurnOutcome::Interrupted {
                finish_reason: FinishReason::Error("Cancelled".to_string()),
            };
        }

        let Some(completion) = completion else {
            let error = stream_error
                .unwrap_or(RuntimeError::ProviderStream(StreamError::UnexpectedEnd))
                .to_string();
            self.finish_interrupted_turn(
                ctx.messages,
                partial_blocks,
                ready_results,
                tool_tasks,
                event_tx,
                Some(error.clone()),
            )
            .await;
            return TurnOutcome::Interrupted {
                finish_reason: FinishReason::Error(error),
            };
        };

        let finish_reason = completion.finish_reason.clone();
        let assistant_index = if completion.message.content.is_empty() {
            None
        } else {
            let message = completion.message;
            let _ = event_tx
                .send(EngineToRuntimeEvent::MessageProduced(message.clone()))
                .await;
            ctx.messages.push(message);
            Some(ctx.messages.len() - 1)
        };

        let (mut results, tasks_cancelled) = match drain_tasks(
            tool_tasks,
            ready_results,
            event_tx,
            cancelled,
            &self.cancel_notify,
        )
        .await
        {
            ToolDrain::Complete(results) => (results, false),
            ToolDrain::Cancelled(results) => (results, true),
        };

        if tasks_cancelled && let Some(index) = assistant_index {
            results = reconcile_cancelled(&ctx.messages[index], results, event_tx).await;
        }

        if orphaned_tool_activity(assistant_index, &finish_reason, &results) {
            let error = ORPHANED_TOOL_ACTIVITY.to_string();
            let _ = event_tx
                .send(EngineToRuntimeEvent::Error(error.clone()))
                .await;
            let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
            return TurnOutcome::Interrupted {
                finish_reason: FinishReason::Error(error),
            };
        }

        if tasks_cancelled {
            commit_results(ctx.messages, results, event_tx).await;
            let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
            return TurnOutcome::Interrupted {
                finish_reason: FinishReason::Error("Cancelled".to_string()),
            };
        }

        if let Some(index) = assistant_index {
            results = align_results(&ctx.messages[index], results);
        }

        let stop_after_permission_denial = should_stop_after_denial(
            ctx.runtime_context
                .as_ref()
                .map(|runtime| runtime.thread_type.as_str()),
            &results,
        );
        commit_results(ctx.messages, results, event_tx).await;

        let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
        TurnOutcome::Completed {
            finish_reason,
            requested_finalization,
            stop_after_permission_denial,
        }
    }

    async fn finish_interrupted_turn(
        &self,
        messages: &mut Vec<Message>,
        partial_blocks: Vec<ContentBlock>,
        mut ready_results: Vec<ToolRunResult>,
        tool_tasks: &mut JoinSet<ToolRunResult>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
        error: Option<String>,
    ) {
        if let Some(error) = error {
            let _ = event_tx.send(EngineToRuntimeEvent::Error(error)).await;
        }
        self.tool_pause_resolver.drain_pending_tool_pauses();

        let completed = abort_tasks(tool_tasks, event_tx).await;
        if !partial_blocks.is_empty() {
            ready_results.extend(completed);

            let assistant = Message::new(Role::Assistant, partial_blocks);
            let _ = event_tx
                .send(EngineToRuntimeEvent::MessageProduced(assistant.clone()))
                .await;
            messages.push(assistant);

            let results = reconcile_cancelled(
                messages.last().expect("assistant message was just pushed"),
                ready_results,
                event_tx,
            )
            .await;
            commit_results(messages, results, event_tx).await;
        }

        let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
    }
}

fn rejection_reason(
    tool_use: &ToolUseBlock,
    mode: TurnMode,
    requested_finalization: &mut Option<FinalizationReason>,
    repeat_guard: &mut RepeatGuard,
) -> Option<String> {
    if mode.is_finalization() {
        return Some("Tool execution is disabled during the finalization turn.".to_string());
    }
    if requested_finalization.is_some() {
        return Some(
            "Tool execution was blocked because the current query is entering finalization."
                .to_string(),
        );
    }
    if repeat_guard.observe(&tool_use.name, &tool_use.input) {
        *requested_finalization = Some(FinalizationReason::RepeatedToolCall);
        return Some(format!(
            "Tool execution was blocked because the same tool and arguments were requested {REPEAT_LIMIT} consecutive times."
        ));
    }
    None
}

fn push_text_delta(blocks: &mut Vec<ContentBlock>, delta: &str) {
    if let Some(ContentBlock::Text(text)) = blocks.last_mut() {
        text.text.push_str(delta);
    } else {
        blocks.push(ContentBlock::Text(TextBlock {
            text: delta.to_string(),
        }));
    }
}

fn push_thinking_delta(blocks: &mut Vec<ContentBlock>, delta: &str) {
    if let Some(ContentBlock::Thinking(thinking)) = blocks.last_mut() {
        thinking.thinking.push_str(delta);
    } else {
        blocks.push(ContentBlock::Thinking(ThinkingBlock {
            thinking: delta.to_string(),
        }));
    }
}

fn should_stop_after_denial(thread_type: Option<&str>, results: &[ToolRunResult]) -> bool {
    thread_type == Some("main")
        && results
            .iter()
            .any(|result| denied_without_note(&result.block))
}

fn orphaned_tool_activity(
    assistant_index: Option<usize>,
    finish_reason: &FinishReason,
    results: &[ToolRunResult],
) -> bool {
    assistant_index.is_none()
        && (matches!(finish_reason, FinishReason::ToolUse) || !results.is_empty())
}

fn denied_without_note(result: &ToolResultBlock) -> bool {
    let Some(metadata) = &result.metadata else {
        return false;
    };

    metadata
        .get("permission_denial_source")
        .and_then(|value| value.as_str())
        == Some("user")
        && metadata
            .get("permission_denied")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        && !metadata
            .get("user_note_present")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tool::ToolRunResult;
    use crate::types::events::EngineToRuntimeEvent;
    use omini_domain::message::{ContentBlock, Role, ToolResultBlock, ToolUseBlock};
    use std::collections::HashMap;
    use tokio::sync::mpsc;
    use tokio::task::JoinSet;

    fn permission_denied_tool_result(user_note_present: bool) -> ToolRunResult {
        let mut metadata = serde_json::Map::new();
        metadata.insert("permission_denied".to_string(), serde_json::json!(true));
        metadata.insert(
            "user_note_present".to_string(),
            serde_json::json!(user_note_present),
        );
        metadata.insert(
            "permission_denial_source".to_string(),
            serde_json::json!("user"),
        );

        ToolRunResult::new(
            ToolResultBlock {
                tool_use_id: "toolu_denied".to_string(),
                is_error: true,
                content: "Permission denied for tool: bash".to_string(),
                metadata: Some(metadata),
            },
            None,
        )
    }

    fn text_from_message(message: &Message) -> String {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn main_user_permission_denial_without_note_stops_next_query() {
        let results = vec![permission_denied_tool_result(false)];

        assert!(should_stop_after_denial(Some("main"), &results));
    }

    #[test]
    fn main_user_permission_denial_with_note_continues_next_query() {
        let results = vec![permission_denied_tool_result(true)];

        assert!(!should_stop_after_denial(Some("main"), &results));
    }

    #[test]
    fn subagent_user_permission_denial_without_note_continues_next_query() {
        let results = vec![permission_denied_tool_result(false)];

        assert!(!should_stop_after_denial(Some("subagent"), &results));
    }

    #[test]
    fn configured_permission_denial_continues_next_query() {
        let results = vec![ToolRunResult::new(
            ToolResultBlock {
                tool_use_id: "toolu_denied".to_string(),
                is_error: true,
                content: "denied by config".to_string(),
                metadata: None,
            },
            None,
        )];

        assert!(!should_stop_after_denial(Some("main"), &results));
    }

    #[test]
    fn completed_turn_requires_assistant_message_for_tool_activity() {
        let result = ToolRunResult::new(
            ToolResultBlock {
                tool_use_id: "toolu_image".to_string(),
                is_error: false,
                content: "Loaded image".to_string(),
                metadata: None,
            },
            None,
        );

        assert!(orphaned_tool_activity(None, &FinishReason::ToolUse, &[]));
        assert!(orphaned_tool_activity(None, &FinishReason::Stop, &[result]));
        assert!(!orphaned_tool_activity(
            Some(0),
            &FinishReason::ToolUse,
            &[]
        ));
    }

    #[tokio::test]
    async fn interrupted_turn_preserves_partial_text() {
        let engine = QueryEngine::default();
        let (tx, mut rx) = mpsc::channel(16);
        let mut messages = Vec::new();
        let mut tool_tasks = JoinSet::new();

        engine
            .finish_interrupted_turn(
                &mut messages,
                vec![ContentBlock::from_text("partial answer".to_string())],
                Vec::new(),
                &mut tool_tasks,
                &tx,
                Some("Stream ended unexpectedly".to_string()),
            )
            .await;

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(text_from_message(&messages[0]), "partial answer");

        let mut saw_error = false;
        let mut saw_message = false;
        let mut saw_turn_end = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                EngineToRuntimeEvent::Error(error) => {
                    saw_error = error == "Stream ended unexpectedly";
                }
                EngineToRuntimeEvent::MessageProduced(message) => {
                    saw_message = text_from_message(&message) == "partial answer";
                }
                EngineToRuntimeEvent::TurnEnded => {
                    saw_turn_end = true;
                }
                _ => {}
            }
        }

        assert!(saw_error);
        assert!(saw_message);
        assert!(saw_turn_end);
    }

    #[tokio::test]
    async fn interrupted_turn_without_partial_text_does_not_create_message() {
        let engine = QueryEngine::default();
        let (tx, mut rx) = mpsc::channel(16);
        let mut messages = Vec::new();
        let mut tool_tasks = JoinSet::new();

        engine
            .finish_interrupted_turn(
                &mut messages,
                Vec::new(),
                Vec::new(),
                &mut tool_tasks,
                &tx,
                Some("Stream ended unexpectedly".to_string()),
            )
            .await;

        assert!(messages.is_empty());

        let mut saw_message = false;
        let mut saw_turn_end = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                EngineToRuntimeEvent::MessageProduced(_) => saw_message = true,
                EngineToRuntimeEvent::TurnEnded => saw_turn_end = true,
                _ => {}
            }
        }

        assert!(!saw_message);
        assert!(saw_turn_end);
    }

    #[tokio::test]
    async fn interrupted_turn_fills_cancelled_tool_results() {
        let engine = QueryEngine::default();
        let (tx, mut rx) = mpsc::channel(16);
        let mut messages = Vec::new();
        let mut tool_tasks = JoinSet::new();
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "read".to_string(),
            input: HashMap::new(),
        };

        engine
            .finish_interrupted_turn(
                &mut messages,
                vec![ContentBlock::ToolUse(tool_use)],
                Vec::new(),
                &mut tool_tasks,
                &tx,
                Some("Stream ended unexpectedly".to_string()),
            )
            .await;

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[1].role, Role::User);

        let tool_result = messages[1]
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolResult(result) => Some(result),
                _ => None,
            })
            .expect("cancelled tool result should be appended");
        assert_eq!(tool_result.tool_use_id, "toolu_1");
        assert!(tool_result.is_error);
        assert_eq!(tool_result.content, "Execution cancelled");

        let mut saw_tool_result_event = false;
        let mut saw_tool_results_message = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                EngineToRuntimeEvent::ToolResult(result) => {
                    saw_tool_result_event = result.tool_use_id == "toolu_1";
                }
                EngineToRuntimeEvent::ToolResultsProduced(message) => {
                    saw_tool_results_message = message.role == Role::User;
                }
                _ => {}
            }
        }

        assert!(saw_tool_result_event);
        assert!(saw_tool_results_message);
    }
}
