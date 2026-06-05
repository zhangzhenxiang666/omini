use super::manual_compact::persist_compact_summary_event;
use super::service::AgentRuntime;
use super::usage::{
    record_total_usage_and_notify, record_total_usage_snapshot, record_usage_snapshot,
};
use super::*;

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

        tokio::spawn(async move {
            let mut proposed_plan_forwarder = plan::ProposedPlanForwarder::new(active_profile);
            while let Some(event) = engine_rx.recv().await {
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
                            &persistence_tx,
                        )
                        .await;
                    }
                    // ===== 透传事件 =====
                    EngineToRuntimeEvent::TurnStarted => {
                        let _ = event_tx.send(RuntimeToServerEvent::TurnStarted).await;
                    }
                    EngineToRuntimeEvent::TurnEnded => {
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
                        let _ = event_tx.send(RuntimeToServerEvent::ToolUse(tu)).await;
                    }
                    EngineToRuntimeEvent::ToolResult(tr) => {
                        let _ = event_tx.send(RuntimeToServerEvent::ToolResult(tr)).await;
                    }
                    EngineToRuntimeEvent::ToolPauseRequested(req) => {
                        if Self::should_auto_approve_permission_pause(&active_profile_handle, &req)
                        {
                            if let Err(e) = tool_pause_resolver.resolve_tool_pause(
                                &req.tool_use_id,
                                ToolPauseResponse::Permission {
                                    approved: true,
                                    note: None,
                                },
                            ) {
                                let _ = event_tx.send(RuntimeToServerEvent::error(e)).await;
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
                        // TODO(compact): 收缩操作暂不通知 UI，后续再决定是否记录内部状态。
                    }
                    EngineToRuntimeEvent::CompactSummaryStarted(event) => {
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
                        persist_compact_summary_event(&session_id, &event, &persistence_tx).await;
                        let _ = event_tx
                            .send(RuntimeToServerEvent::CompactSummaryFinished(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                        let _ = event_tx
                            .send(RuntimeToServerEvent::CompactSummaryFailed(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage) => {
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
                        let _ = event_tx
                            .send(RuntimeToServerEvent::SubagentStarted(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentSessionCreated(session) => {
                        let _ = persistence_tx
                            .send(RuntimePersistenceEvent::CreateSession(session))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentUsageRecorded {
                        session_id: subagent_session_id,
                        usage,
                    } => {
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
                            record_total_usage_snapshot(&usage_state, usage, context_window).await;
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
                                &persistence_tx,
                            )
                            .await;
                        } else {
                            history::persist_ui_message(
                                &event.session_id,
                                &subagent_blocks_dir,
                                &event.message,
                                &persistence_tx,
                            )
                            .await;
                        }
                        let _ = event_tx
                            .send(RuntimeToServerEvent::SubagentMessageProduced(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentToolUse(event) => {
                        let _ = event_tx
                            .send(RuntimeToServerEvent::SubagentToolUse(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentToolResult(event) => {
                        let _ = event_tx
                            .send(RuntimeToServerEvent::SubagentToolResult(event))
                            .await;
                    }
                    EngineToRuntimeEvent::SubagentFinished(event) => {
                        let _ = event_tx
                            .send(RuntimeToServerEvent::SubagentFinished(event))
                            .await;
                    }
                    EngineToRuntimeEvent::Error(e) => {
                        let _ = event_tx.send(RuntimeToServerEvent::error(e)).await;
                    }
                    EngineToRuntimeEvent::Warning(warning) => {
                        let _ = event_tx.send(RuntimeToServerEvent::warning(warning)).await;
                    }
                }
            }
        })
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
