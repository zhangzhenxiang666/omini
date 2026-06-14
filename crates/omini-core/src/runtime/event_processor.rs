use super::manual_compact::persist_compact_summary_event;
use super::service::AgentRuntime;
use super::usage::{
    record_total_usage_and_notify, record_total_usage_snapshot, record_usage_snapshot,
};
use super::*;
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
        let session_id = self
            .session_id
            .clone()
            .expect("session must exist before processing events");
        let session_dir = self
            .session_dir
            .clone()
            .expect("session dir must exist before processing events");
        let event_tx = self.event_tx.clone();
        let persistence_tx = self.persistence_tx.clone();
        let usage_state = Arc::clone(&self.session_usage);
        let project = self.project.clone();
        let blocks_dir = session_dir.path().join("blocks");
        let context_window = self.current_context_window();
        let span_session_id = session_id.clone();

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
                                &session_dir,
                                &session_id,
                                &blocks_dir,
                                message.clone(),
                                active,
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
                        EngineToRuntimeEvent::LlmHistoryProduced(msg) => {
                            history::persist_llm_history_only(&session_dir, &msg);
                        }
                        EngineToRuntimeEvent::ToolResultsDisplayProduced(msg) => {
                            history::persist_ui_message(
                                &session_id,
                                &blocks_dir,
                                &msg,
                                active,
                                &persistence_tx,
                            )
                            .await;
                        }
                        EngineToRuntimeEvent::MessageProduced(msg)
                        | EngineToRuntimeEvent::ToolResultsProduced(msg) => {
                            history::persist_one(
                                &session_dir,
                                &session_id,
                                &blocks_dir,
                                msg,
                                active,
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
                                source_session_id = ?req.source_session_id,
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
                                .send(RuntimeToServerEvent::ToolPauseRequested(req))
                                .await;
                        }
                        EngineToRuntimeEvent::PlanSubmitted(plan) => {
                            let _ = event_tx
                                .send(RuntimeToServerEvent::PlanSubmitted(plan))
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
                                .send(RuntimePersistenceEvent::RecordSessionUsage {
                                    session_id: session_id.clone(),
                                    usage,
                                })
                                .await;
                            let snapshot =
                                record_usage_snapshot(&usage_state, usage, context_window).await;
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
                                compact_session_id = ?event.session_id,
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
                                compact_session_id = ?event.session_id,
                                agent_label = ?event.agent_label,
                                summary_chars = event.summary.chars().count(),
                                after_tokens = event.after_tokens,
                                "compact summary finished"
                            );
                            persist_compact_summary_event(&session_id, &event, &persistence_tx)
                                .await;
                            let _ = event_tx
                                .send(RuntimeToServerEvent::CompactSummaryFinished(event))
                                .await;
                        }
                        EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                            tracing::warn!(
                                trigger = %event.trigger,
                                compact_session_id = ?event.session_id,
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
                                &session_id,
                                usage,
                                &event_tx,
                                &persistence_tx,
                                &usage_state,
                            )
                            .await;
                        }
                        EngineToRuntimeEvent::SubagentStarted(event) => {
                            tracing::debug!(event = ?event, "subagent started");
                            let _ = event_tx
                                .send(RuntimeToServerEvent::SubagentStarted(event))
                                .await;
                        }
                        EngineToRuntimeEvent::SubagentSessionCreated(session) => {
                            tracing::debug!(
                                subagent_session_id = %session.id,
                                parent_session_id = ?session.parent_session_id,
                                agent_label = ?session.agent_label,
                                "subagent session created"
                            );
                            let _ = persistence_tx
                                .send(RuntimePersistenceEvent::CreateSession(session))
                                .await;
                        }
                        EngineToRuntimeEvent::SubagentUsageRecorded {
                            session_id: subagent_session_id,
                            usage,
                        } => {
                            tracing::debug!(
                                subagent_session_id = %subagent_session_id,
                                prompt_tokens = usage.prompt_tokens,
                                completion_tokens = usage.completion_tokens,
                                cached_tokens = usage.cached_tokens,
                                "recording subagent usage"
                            );
                            let _ = persistence_tx
                                .send(RuntimePersistenceEvent::RecordSessionUsage {
                                    session_id: subagent_session_id,
                                    usage,
                                })
                                .await;
                            let _ = persistence_tx
                                .send(RuntimePersistenceEvent::RecordParentSubagentUsage {
                                    session_id: session_id.clone(),
                                    usage,
                                })
                                .await;
                            let snapshot =
                                record_total_usage_snapshot(&usage_state, usage, context_window)
                                    .await;
                            let _ = event_tx
                                .send(RuntimeToServerEvent::UsageChanged(snapshot))
                                .await;
                        }
                        EngineToRuntimeEvent::SubagentMessageProduced(event) => {
                            let parent_dir = project.session(&session_id);
                            let subagent_dir = parent_dir.subagent(&event.session_id);
                            let subagent_blocks_dir = subagent_dir.path().join("blocks");
                            if event.persist_llm_history {
                                history::persist_one(
                                    &subagent_dir,
                                    &event.session_id,
                                    &subagent_blocks_dir,
                                    event.message.clone(),
                                    active,
                                    &persistence_tx,
                                )
                                .await;
                            } else {
                                history::persist_ui_message(
                                    &event.session_id,
                                    &subagent_blocks_dir,
                                    &event.message,
                                    active,
                                    &persistence_tx,
                                )
                                .await;
                            }
                            let _ = event_tx
                                .send(RuntimeToServerEvent::SubagentMessageProduced(event))
                                .await;
                        }
                        EngineToRuntimeEvent::SubagentToolUse(event) => {
                            tracing::debug!(
                                subagent_session_id = %event.session_id,
                                tool_use_id = %event.tool_use.id,
                                tool_name = %event.tool_use.name,
                                "subagent tool use"
                            );
                            let _ = event_tx
                                .send(RuntimeToServerEvent::SubagentToolUse(event))
                                .await;
                        }
                        EngineToRuntimeEvent::SubagentToolResult(event) => {
                            tracing::debug!(
                                subagent_session_id = %event.session_id,
                                tool_use_id = %event.tool_result.tool_use_id,
                                is_error = event.tool_result.is_error,
                                "subagent tool result"
                            );
                            let _ = event_tx
                                .send(RuntimeToServerEvent::SubagentToolResult(event))
                                .await;
                        }
                        EngineToRuntimeEvent::SubagentFinished(event) => {
                            tracing::debug!(event = ?event, "subagent finished");
                            let _ = event_tx
                                .send(RuntimeToServerEvent::SubagentFinished(event))
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
                session_id = %span_session_id
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
