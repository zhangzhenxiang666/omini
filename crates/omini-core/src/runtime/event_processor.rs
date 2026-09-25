use super::manual_compact::persist_compact_summary_event;
use super::service::AgentRuntime;
use super::usage::{record_total_usage_and_notify, record_usage_snapshot};
use super::*;
use tracing::Instrument;

impl AgentRuntime {
    /// 启动事件处理器。
    #[cfg(test)]
    #[allow(dead_code)]
    pub async fn spawn_event_processor(
        &self,
        engine_rx: mpsc::Receiver<EngineToRuntimeEvent>,
        active_profile: ActiveProfile,
        active_profile_handle: Arc<RwLock<ActiveProfile>>,
        tool_pause_resolver: ToolPauseResolver,
    ) -> tokio::task::JoinHandle<()> {
        self.spawn_event_processor_for_run(
            engine_rx,
            active_profile,
            active_profile_handle,
            tool_pause_resolver,
            None,
        )
        .await
    }

    pub async fn spawn_event_processor_for_run(
        &self,
        mut engine_rx: mpsc::Receiver<EngineToRuntimeEvent>,
        active_profile: ActiveProfile,
        active_profile_handle: Arc<RwLock<ActiveProfile>>,
        tool_pause_resolver: ToolPauseResolver,
        run_id: Option<String>,
    ) -> tokio::task::JoinHandle<()> {
        let thread_id = self.thread_id.clone();
        let event_tx = self.event_tx.clone();
        let persistence_tx = self.persistence_tx.clone();
        let usage_state = Arc::clone(&self.thread_usage);
        let model = self.settings.active_model();
        let model_ref = format!("{}/{}", model.provider_id, model.model_id);
        let context_window = active_run::current_context_window(&self.settings);
        let task_supervisor = Arc::clone(&self.task_supervisor);
        let span_thread_id = thread_id.clone();
        let run_id_for_events = run_id.clone();

        tokio::spawn(
            async move {
                let mut proposed_plan_forwarder = plan::ProposedPlanForwarder::new(active_profile);
                let mut next_step_no = 0_u32;
                let mut active_step_id: Option<String> = None;
                let mut tool_uses = std::collections::HashMap::new();
                while let Some(event) = engine_rx.recv().await {
                    let active = *active_profile_handle
                        .read()
                        .expect("active profile lock poisoned");
                    match event {
                        // ===== 需要持久化的事件 =====
                        EngineToRuntimeEvent::UserMessageProduced(message) => {
                            history::persist_llm_history_only(
                                &thread_id,
                                &message,
                                &persistence_tx,
                            )
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
                            if let Some(run_id) = &run_id_for_events {
                                next_step_no += 1;
                                let step_id = uuid::Uuid::new_v4().to_string();
                                let started_at = chrono::Utc::now();
                                active_step_id = Some(step_id.clone());
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpsertAgentStep {
                                        step: omini_domain::agent_run::AgentStepSnapshot {
                                            id: step_id,
                                            run_id: run_id.clone(),
                                            step_no: next_step_no,
                                            status: omini_domain::agent_run::AgentStepStatus::Running,
                                            started_at,
                                            finished_at: None,
                                            input_tokens: 0,
                                            output_tokens: 0,
                                        },
                                    })
                                    .await;
                            }
                            let _ = event_tx.send(RuntimeToServerEvent::TurnStarted).await;
                        }
                        EngineToRuntimeEvent::TurnEnded => {
                            tracing::debug!("forwarding turn ended event");
                            if let Some(step_id) = active_step_id.take() {
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpdateAgentStep {
                                        step_id,
                                        status: omini_domain::agent_run::AgentStepStatus::Completed,
                                        finished_at: Some(chrono::Utc::now()),
                                        add_input_tokens: 0,
                                        add_output_tokens: 0,
                                    })
                                    .await;
                            }
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
                            if let (Some(step_id), Some(run_id)) =
                                (active_step_id.as_ref(), run_id_for_events.as_ref())
                            {
                                let record = omini_domain::agent_run::ToolUseExecutionSnapshot {
                                    id: tu.id.clone(),
                                    step_id: step_id.clone(),
                                    name: tu.name.clone(),
                                    input: serde_json::to_value(&tu.input)
                                        .unwrap_or(serde_json::Value::Null),
                                    status: omini_domain::agent_run::ToolUseStatus::Running,
                                    updated_at: chrono::Utc::now(),
                                };
                                tool_uses.insert(tu.id.clone(), record.clone());
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpsertToolUseExecution {
                                        tool_use: record,
                                        status: omini_domain::agent_run::ToolUseStatus::Running,
                                    })
                                    .await;
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpdateAgentRun {
                                        run_id: run_id.clone(),
                                        status: omini_domain::agent_run::AgentRunStatus::Running,
                                        started_at: None,
                                        finished_at: None,
                                        add_tokens: 0,
                                    })
                                    .await;
                            }
                            let _ = event_tx.send(RuntimeToServerEvent::ToolUse(tu)).await;
                        }
                        EngineToRuntimeEvent::ToolResult(tr) => {
                            tracing::debug!(
                                tool_use_id = %tr.tool_use_id,
                                is_error = tr.is_error,
                                "forwarding tool result event"
                            );
                            if let Some(mut record) = tool_uses.get(&tr.tool_use_id).cloned() {
                                record.updated_at = chrono::Utc::now();
                                let status = if tr.is_error {
                                    omini_domain::agent_run::ToolUseStatus::Failed
                                } else {
                                    omini_domain::agent_run::ToolUseStatus::Completed
                                };
                                record.status = status;
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpsertToolUseExecution {
                                        tool_use: record,
                                        status,
                                    })
                                    .await;
                            }
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
                            if let Some(mut record) = tool_uses.get(&req.tool_use_id).cloned()
                                && matches!(req.kind, omini_domain::events::ToolPauseKind::Permission(_))
                                && let Some(run_id) = &run_id_for_events
                            {
                                record.updated_at = chrono::Utc::now();
                                record.status = omini_domain::agent_run::ToolUseStatus::WaitingApproval;
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpsertToolUseExecution {
                                        tool_use: record,
                                        status: omini_domain::agent_run::ToolUseStatus::WaitingApproval,
                                    })
                                    .await;
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpdateAgentRun {
                                        run_id: run_id.clone(),
                                        status: omini_domain::agent_run::AgentRunStatus::WaitingApproval,
                                        started_at: None,
                                        finished_at: None,
                                        add_tokens: 0,
                                    })
                                    .await;
                            }
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
                            if let Some(run_id) = &run_id_for_events {
                                let _ = persistence_tx
                                    .send(RuntimePersistenceEvent::UpdateAgentRun {
                                        run_id: run_id.clone(),
                                        status: omini_domain::agent_run::AgentRunStatus::Running,
                                        started_at: None,
                                        finished_at: None,
                                        add_tokens: usage.total_tokens() as i64,
                                    })
                                    .await;
                                if let Some(step_id) = &active_step_id {
                                    let _ = persistence_tx
                                        .send(RuntimePersistenceEvent::UpdateAgentStep {
                                            step_id: step_id.clone(),
                                            status: omini_domain::agent_run::AgentStepStatus::Running,
                                            finished_at: None,
                                            add_input_tokens: usage.prompt_tokens as i64,
                                            add_output_tokens: usage.completion_tokens as i64,
                                        })
                                        .await;
                                }
                            }
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
