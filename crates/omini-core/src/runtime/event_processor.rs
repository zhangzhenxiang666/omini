use super::manual_compact::persist_compact_summary_event;
use super::service::AgentRuntime;
use super::usage::{record_total_usage_and_notify, record_usage_snapshot};
use super::*;
use omini_domain::display::HistoryItem;
use tracing::Instrument;

impl AgentRuntime {
    /// 启动事件处理器。
    pub(super) async fn spawn_event_processor(
        &self,
        mut engine_rx: mpsc::Receiver<EngineToRuntimeEvent>,
        active_profile: ActiveProfile,
        active_profile_handle: Arc<RwLock<ActiveProfile>>,
        tool_pause_resolver: ToolPauseResolver,
    ) -> tokio::task::JoinHandle<()> {
        let thread_id = self.thread_id.clone();
        let event_tx = self.event_tx.clone();
        let persistence_tx = self.persistence_tx.clone();
        let usage_state = Arc::clone(&self.thread_usage);
        let model_ref = format!("{}/{}", self.settings.active_provider, self.settings.model);
        let context_window = active_run::current_context_window(&self.settings);
        let task_supervisor = Arc::clone(&self.task_supervisor);
        let span_thread_id = thread_id.clone();

        tokio::spawn(
            async move {
                let mut proposed_plan_forwarder = plan::ProposedPlanForwarder::new(active_profile);
                while let Some(event) = engine_rx.recv().await {
                    let active = *active_profile_handle
                        .read()
                        .expect("active profile lock poisoned");
                    match event {
                        // ===== 需要持久化的事件 =====
                        EngineToRuntimeEvent::UserMessageProduced {
                            message,
                            client_echo_id,
                        } => {
                            history::persist_one(
                                &thread_id,
                                message.clone(),
                                active,
                                &model_ref,
                                &persistence_tx,
                            )
                            .await;
                            let _ = event_tx
                                .send(RuntimeToServerEvent::UserMessageInjected {
                                    item: HistoryItem::Message(message),
                                    client_echo_id,
                                })
                                .await;
                        }
                        EngineToRuntimeEvent::AgentTaskNotificationsProduced {
                            notification,
                            llm_message,
                            task_ids,
                            ack,
                        } => {
                            let (persistence_ack, persistence_result) =
                                tokio::sync::oneshot::channel();
                            let result = if persistence_tx
                                .send(RuntimePersistenceEvent::InsertAgentTaskNotification {
                                    owner_thread_id: thread_id.clone(),
                                    notification: notification.clone(),
                                    llm_message,
                                    task_ids: task_ids.clone(),
                                    created_at: Utc::now(),
                                    ack: persistence_ack,
                                })
                                .await
                                .is_err()
                            {
                                Err("agent task notification persistence channel closed"
                                    .to_string())
                            } else {
                                persistence_result
                                    .await
                                    .map_err(|_| {
                                        "agent task notification acknowledgement dropped"
                                            .to_string()
                                    })
                                    .and_then(|result| result)
                            };
                            if result.is_ok() {
                                task_supervisor.mark_notifications_delivered(&task_ids);
                                let _ = event_tx
                                    .send(RuntimeToServerEvent::UserMessageInjected {
                                        item: HistoryItem::AgentTaskNotification(notification),
                                        client_echo_id: None,
                                    })
                                    .await;
                            }
                            let _ = ack.send(result);
                        }
                        EngineToRuntimeEvent::LlmHistoryProduced(msg) => {
                            history::persist_llm_history_only(&thread_id, &msg, &persistence_tx)
                                .await;
                        }
                        EngineToRuntimeEvent::ReplaceLlmContext {
                            thread_id: compacted_thread_id,
                            expected_version,
                            messages,
                            ack,
                        } => {
                            let _ = persistence_tx
                                .send(RuntimePersistenceEvent::ReplaceLlmContext {
                                    thread_id: compacted_thread_id,
                                    expected_version,
                                    messages,
                                    created_at: Utc::now(),
                                    ack,
                                })
                                .await;
                        }
                        EngineToRuntimeEvent::ToolResultsDisplayProduced(msg) => {
                            history::persist_ui_message(
                                &thread_id,
                                &msg,
                                active,
                                &model_ref,
                                &persistence_tx,
                            )
                            .await;
                        }
                        EngineToRuntimeEvent::MessageProduced(msg)
                        | EngineToRuntimeEvent::ToolResultsProduced(msg) => {
                            history::persist_one(
                                &thread_id,
                                msg,
                                active,
                                &model_ref,
                                &persistence_tx,
                            )
                            .await;
                        }
                        // ===== 透传事件 =====
                        EngineToRuntimeEvent::TurnStarted => {
                            tracing::debug!("forwarding turn started event");
                            let _ = event_tx.send(RuntimeToServerEvent::TurnStarted).await;
                        }
                        EngineToRuntimeEvent::TurnEnded => {
                            tracing::debug!("forwarding turn ended event");
                            proposed_plan_forwarder.flush(&event_tx).await;
                            let _ = event_tx.send(RuntimeToServerEvent::TurnEnded).await;
                        }
                        EngineToRuntimeEvent::ThinkingDelta(t) => {
                            let _ = event_tx.send(RuntimeToServerEvent::ThinkingDelta(t)).await;
                        }
                        EngineToRuntimeEvent::TextDelta(t) => {
                            proposed_plan_forwarder
                                .forward_text_delta(&event_tx, t)
                                .await;
                        }
                        EngineToRuntimeEvent::ToolUse(tu) => {
                            tracing::debug!(
                                tool_use_id = %tu.id,
                                tool_name = %tu.name,
                                "forwarding tool use event"
                            );
                            let _ = event_tx.send(RuntimeToServerEvent::ToolUse(tu)).await;
                        }
                        EngineToRuntimeEvent::ToolResult(tr) => {
                            tracing::debug!(
                                tool_use_id = %tr.tool_use_id,
                                is_error = tr.is_error,
                                "forwarding tool result event"
                            );
                            let _ = event_tx.send(RuntimeToServerEvent::ToolResult(tr)).await;
                        }
                        EngineToRuntimeEvent::ToolPauseRequested(req) => {
                            tracing::debug!(
                                tool_use_id = %req.tool_use_id,
                                tool_name = %req.tool_name,
                                source_thread_id = ?req.source_thread_id,
                                source_agent_label = ?req.source_agent_label,
                                pause_kind = ?req.kind,
                                "tool pause requested"
                            );
                            if Self::should_auto_approve_permission_pause(
                                &active_profile_handle,
                                &req,
                            ) {
                                tracing::debug!(
                                    tool_use_id = %req.tool_use_id,
                                    tool_name = %req.tool_name,
                                    "auto approving permission pause"
                                );
                                if let Err(e) = tool_pause_resolver.resolve_tool_pause(
                                    &req.tool_use_id,
                                    ToolPauseResponse::Permission {
                                        approved: true,
                                        note: None,
                                    },
                                ) {
                                    tracing::warn!(
                                        tool_use_id = %req.tool_use_id,
                                        error = %e,
                                        "failed to auto approve permission pause"
                                    );
                                    let _ = event_tx
                                        .send(RuntimeToServerEvent::error(e.to_string()))
                                        .await;
                                }
                                continue;
                            }
                            let _ = event_tx
                                .send(RuntimeToServerEvent::ToolPauseRequested(*req))
                                .await;
                        }
                        EngineToRuntimeEvent::UsageRecorded(usage) => {
                            tracing::debug!(
                                prompt_tokens = usage.prompt_tokens,
                                completion_tokens = usage.completion_tokens,
                                cached_tokens = usage.cached_tokens,
                                "recording usage"
                            );
                            let _ = persistence_tx
                                .send(RuntimePersistenceEvent::RecordThreadUsage {
                                    thread_id: thread_id.clone(),
                                    usage,
                                })
                                .await;
                            let snapshot =
                                record_usage_snapshot(&usage_state, usage, context_window);
                            let _ = event_tx
                                .send(RuntimeToServerEvent::UsageChanged(snapshot))
                                .await;
                        }
                        EngineToRuntimeEvent::CompactShrinkStarted(_)
                        | EngineToRuntimeEvent::CompactShrinkFinished(_)
                        | EngineToRuntimeEvent::CompactShrinkFailed(_) => {
                            tracing::debug!("compact shrink event received");
                            // TODO(compact): 收缩操作暂不通知 UI，后续再决定是否记录内部状态。
                        }
                        EngineToRuntimeEvent::CompactSummaryStarted(event) => {
                            tracing::debug!(
                                trigger = %event.trigger,
                                compact_thread_id = ?event.thread_id,
                                agent_label = ?event.agent_label,
                                "compact summary started"
                            );
                            let _ = event_tx
                                .send(RuntimeToServerEvent::CompactSummaryStarted(event))
                                .await;
                        }
                        EngineToRuntimeEvent::CompactSummaryDelta(event) => {
                            let _ = event_tx
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
                                "compact summary finished"
                            );
                            persist_compact_summary_event(
                                &thread_id,
                                &event,
                                &model_ref,
                                &persistence_tx,
                            )
                            .await;
                            let _ = event_tx
                                .send(RuntimeToServerEvent::CompactSummaryFinished(event))
                                .await;
                        }
                        EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                            tracing::warn!(
                                trigger = %event.trigger,
                                compact_thread_id = ?event.thread_id,
                                agent_label = ?event.agent_label,
                                message = %event.message,
                                "compact summary failed"
                            );
                            let _ = event_tx
                                .send(RuntimeToServerEvent::CompactSummaryFailed(event))
                                .await;
                        }
                        EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage) => {
                            tracing::debug!(
                                prompt_tokens = usage.prompt_tokens,
                                completion_tokens = usage.completion_tokens,
                                cached_tokens = usage.cached_tokens,
                                "recording compact summary usage"
                            );
                            record_total_usage_and_notify(
                                &thread_id,
                                usage,
                                &event_tx,
                                &persistence_tx,
                                &usage_state,
                            )
                            .await;
                        }
                        EngineToRuntimeEvent::Error(e) => {
                            tracing::warn!(error = %e, "runtime engine error");
                            let _ = event_tx.send(RuntimeToServerEvent::error(e)).await;
                        }
                        EngineToRuntimeEvent::Warning(warning) => {
                            tracing::warn!(warning = %warning, "runtime engine warning");
                            let _ = event_tx.send(RuntimeToServerEvent::warning(warning)).await;
                        }
                    }
                }
            }
            .instrument(tracing::debug_span!(
                "event_processor",
                thread_id = %span_thread_id
            )),
        )
    }

    fn should_auto_approve_permission_pause(
        active_profile_handle: &RwLock<ActiveProfile>,
        req: &ToolPauseRequest,
    ) -> bool {
        let active_profile = *active_profile_handle
            .read()
            .expect("active profile lock poisoned");
        active_profile == ActiveProfile::Auto && matches!(req.kind, ToolPauseKind::Permission(_))
    }
}
