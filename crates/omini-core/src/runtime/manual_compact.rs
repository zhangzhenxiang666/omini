use super::usage::record_total_usage_and_notify;
use super::*;
use crate::error::RuntimeError;
use crate::runtime::compact::{self, CompactRequestContext};
use omini_domain::events::CompactTrigger;
use omini_domain::tool::ToolDefinition;
use omini_provider_api::LlmClient;
use std::sync::Arc;
use tracing::Instrument;

impl AgentRuntime {
    /// 处理手动 compact 请求：构建上下文、执行压缩、处理取消和结果。
    pub async fn handle_compact_context(&mut self, instructions: Option<String>) {
        if self.messages.is_empty() {
            self.send_event(RuntimeToServerEvent::Notification(Notification::warning(
                "没有可压缩的线程历史".to_string(),
            )))
            .await;
            return;
        }

        let subagent_registry = self.capabilities.subagent_registry();
        let skill_registry = self.capabilities.skill_registry();
        let runtime_context = Arc::new(ToolRuntimeContext {
            thread_id: self.thread_id.clone(),
            run_id: None,
            thread_type: "main".to_string(),
            agent_label: None,
            thread_dir: self.thread_dir.clone(),
            llm_context_version: Arc::clone(&self.llm_context_version),
            agent_depth: 0,
            task_id: None,
            owner_thread_id: self.thread_id.clone(),
            agent_registry: subagent_registry,
            skill_registry,
            task_supervisor: Some(Arc::clone(&self.task_supervisor)),
            project: self.project.clone(),
        });
        let tool_definitions = self.tool_registry_snapshot().definitions();
        let cancelled = Arc::clone(&self.cancelled);
        let cancel_notify = self.query_engine.cancel_notify_arc();
        let event_tx = self.event_tx.clone();

        let input = ManualCompactInput {
            settings: &self.settings,
            llm_client: &self.llm_client,
            tool_definitions: &tool_definitions,
            custom_instructions: instructions.as_deref(),
            runtime_context,
        };
        let cancel_token = compact::CompactCancelToken::new(&cancelled, cancel_notify.as_ref());

        let compact_fut = execute_manual_compact(
            &mut self.messages,
            input,
            &event_tx,
            &self.persistence_tx,
            &self.thread_usage,
            cancel_token,
        );
        tokio::pin!(compact_fut);

        loop {
            tokio::select! {
                result = &mut compact_fut => {
                    match result {
                        Ok(outcome) => {
                            tracing::debug!(outcome = ?outcome, "manual compact finished");
                            if compact_outcome_is_noop(&outcome) {
                                let _ = event_tx.send(RuntimeToServerEvent::warning(
                                    "当前线程历史还不需要压缩".to_string(),
                                )).await;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "manual compact failed");
                            let _ = event_tx.send(RuntimeToServerEvent::Notification(
                                Notification::warning(error.to_string()),
                            )).await;
                        }
                    }
                    break;
                }
                Some(req) = self.request_rx.recv() => {
                    match req {
                        ServerToRuntimeEvent::CancelRun => {
                            tracing::debug!("manual compact cancellation requested");
                            self.cancelled.store(true, Ordering::Relaxed);
                            self.query_engine.notify_cancel_waiters();
                        }
                        _ => {
                            tracing::debug!("request ignored during manual compact");
                        }
                    }
                }
            }
        }
    }
}

pub struct ManualCompactInput<'a> {
    pub settings: &'a Settings,
    pub llm_client: &'a LlmClient,
    pub tool_definitions: &'a [ToolDefinition],
    pub custom_instructions: Option<&'a str>,
    pub runtime_context: Arc<ToolRuntimeContext>,
}

/// 执行手动 compact，不借用 `&mut AgentRuntime`，
/// 使调用方可以在 `tokio::select!` 中同时访问 runtime 的其他字段。
pub async fn execute_manual_compact(
    messages: &mut Vec<Message>,
    input: ManualCompactInput<'_>,
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
    usage_state: &Arc<Mutex<ThreadUsageSnapshot>>,
    cancel_token: compact::CompactCancelToken<'_>,
) -> Result<crate::runtime::compact::CompactOutcome, RuntimeError> {
    if messages.is_empty() {
        return Err(RuntimeError::InvalidRequest {
            message: "没有可压缩的线程历史".to_string(),
        });
    }

    let (compact_tx, mut compact_rx) = mpsc::channel(16);
    let forwarder_event_tx = event_tx.clone();
    let forwarder_persistence_tx = persistence_tx.clone();
    let forwarder_usage_state = Arc::clone(usage_state);
    let forwarder_thread_id = input.runtime_context.thread_id.clone();
    let forwarder_model_ref = format!(
        "{}/{}",
        input.settings.active_provider, input.settings.model
    );
    let span_thread_id = forwarder_thread_id.clone();
    let forwarder = tokio::spawn(
        async move {
            while let Some(event) = compact_rx.recv().await {
                match event {
                    EngineToRuntimeEvent::CompactShrinkStarted(_)
                    | EngineToRuntimeEvent::CompactShrinkFinished(_)
                    | EngineToRuntimeEvent::CompactShrinkFailed(_) => {
                        tracing::debug!("manual compact shrink event received");
                    }
                    EngineToRuntimeEvent::CompactSummaryStarted(event) => {
                        tracing::debug!(
                            trigger = %event.trigger,
                            compact_thread_id = ?event.thread_id,
                            agent_label = ?event.agent_label,
                            "manual compact summary started"
                        );
                        let _ = forwarder_event_tx
                            .send(RuntimeToServerEvent::CompactSummaryStarted(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryDelta(event) => {
                        let _ = forwarder_event_tx
                            .send(RuntimeToServerEvent::CompactSummaryDelta(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFinished(event) => {
                        tracing::debug!(
                            trigger = %event.trigger,
                            compact_thread_id = ?event.thread_id,
                            agent_label = ?event.agent_label,
                            summary_chars = event.summary.chars().count(),
                            after_tokens = event.after_tokens,
                            "manual compact summary finished"
                        );
                        persist_compact_summary_event(
                            &forwarder_thread_id,
                            &event,
                            &forwarder_model_ref,
                            &forwarder_persistence_tx,
                        )
                        .await;
                        let _ = forwarder_event_tx
                            .send(RuntimeToServerEvent::CompactSummaryFinished(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                        tracing::warn!(
                            trigger = %event.trigger,
                            compact_thread_id = ?event.thread_id,
                            agent_label = ?event.agent_label,
                            message = %event.message,
                            "manual compact summary failed"
                        );
                        let _ = forwarder_event_tx
                            .send(RuntimeToServerEvent::CompactSummaryFailed(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage) => {
                        tracing::debug!(
                            prompt_tokens = usage.prompt_tokens,
                            completion_tokens = usage.completion_tokens,
                            cached_tokens = usage.cached_tokens,
                            "manual compact usage recorded"
                        );
                        record_total_usage_and_notify(
                            &forwarder_thread_id,
                            usage,
                            &forwarder_event_tx,
                            &forwarder_persistence_tx,
                            &forwarder_usage_state,
                        )
                        .await;
                    }
                    EngineToRuntimeEvent::ReplaceLlmContext {
                        thread_id: compacted_thread_id,
                        expected_version,
                        messages,
                        ack,
                    } => {
                        let _ = forwarder_persistence_tx
                            .send(RuntimePersistenceEvent::ReplaceLlmContext {
                                thread_id: compacted_thread_id,
                                expected_version,
                                messages,
                                created_at: Utc::now(),
                                ack,
                            })
                            .await;
                    }
                    _ => {}
                }
            }
        }
        .instrument(tracing::debug_span!(
            "compact",
            thread_id = %span_thread_id,
            compact_kind = "manual",
            task_kind = "manual_compact_forwarder"
        )),
    );

    let compact_ctx = CompactRequestContext {
        settings: input.settings,
        llm_client: input.llm_client,
        tool_definitions: input.tool_definitions,
        runtime_context: Some(input.runtime_context.as_ref()),
        event_tx: &compact_tx,
        trigger: CompactTrigger::Manual,
        custom_instructions: input.custom_instructions,
        cancel_token,
    };
    let result = compact::force_compact(messages, &compact_ctx).await;
    drop(compact_tx);
    let _ = forwarder.await;

    result
}

pub async fn persist_compact_summary_event(
    thread_id: &str,
    event: &omini_domain::events::CompactSummaryFinishedEvent,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let summary = DisplaySummary {
        id: Uuid::new_v4().to_string(),
        title: "LLM Summary".to_string(),
        markdown: event.summary.clone(),
        created_at: Utc::now(),
    };
    history::persist_compact_summary_ui_message(thread_id, &summary, model_ref, persistence_tx)
        .await;
}

pub fn compact_outcome_is_noop(outcome: &crate::runtime::compact::CompactOutcome) -> bool {
    outcome.before_tokens == outcome.after_tokens
        && outcome.before_messages == outcome.after_messages
}
