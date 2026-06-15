use super::service::AgentRuntime;
use super::usage::record_total_usage_and_notify;
use super::*;
use crate::error::RuntimeError;
use tracing::Instrument;

impl AgentRuntime {
    pub(super) async fn compact_context(&mut self, custom_instructions: Option<&str>) {
        tracing::debug!(
            session_id = ?self.session_id,
            has_custom_instructions = custom_instructions.is_some(),
            "manual compact requested"
        );
        if let Err(error) = self
            .force_compact_current_session(custom_instructions)
            .await
        {
            tracing::warn!(error = %error, "manual compact failed");
            self.send_event(RuntimeToServerEvent::Notification(Notification::warning(
                error.to_string(),
            )))
            .await;
        }
    }

    pub(crate) async fn force_compact_current_session(
        &mut self,
        custom_instructions: Option<&str>,
    ) -> Result<(), RuntimeError> {
        if self.messages.is_empty() {
            return Err(RuntimeError::InvalidRequest {
                message: "没有可压缩的会话历史".to_string(),
            });
        }

        let subagent_registry = self.capabilities.subagent_registry();
        let skill_registry = self.capabilities.skill_registry();
        let runtime_context = Arc::new(ToolRuntimeContext {
            session_id: self.session_id.clone(),
            run_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            session_dir: self.session_dir.clone(),
            subagent_registry,
            skill_registry,
            subagent_runner: Some(Arc::clone(&self.subagent_runner)),
            project: self.project.clone(),
        });
        let (compact_tx, mut compact_rx) = mpsc::channel(16);
        let event_tx = self.event_tx.clone();
        let persistence_tx = self.persistence_tx.clone();
        let usage_state = Arc::clone(&self.session_usage);
        let session_id = self.session_id.clone();
        let span_session_id = session_id.clone();
        let forwarder = tokio::spawn(
            async move {
                while let Some(event) = compact_rx.recv().await {
                    match event {
                        EngineToRuntimeEvent::CompactShrinkStarted(_)
                        | EngineToRuntimeEvent::CompactShrinkFinished(_)
                        | EngineToRuntimeEvent::CompactShrinkFailed(_) => {
                            tracing::debug!("manual compact shrink event received");
                            // TODO(compact): 收缩操作暂不通知 UI，后续再决定是否记录内部状态。
                        }
                        EngineToRuntimeEvent::CompactSummaryStarted(event) => {
                            tracing::debug!(
                                trigger = %event.trigger,
                                compact_session_id = ?event.session_id,
                                agent_label = ?event.agent_label,
                                "manual compact summary started"
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
                                "manual compact summary finished"
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
                                "manual compact summary failed"
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
                                "manual compact usage recorded"
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
                        _ => {}
                    }
                }
            }
            .instrument(tracing::debug_span!(
                "compact",
                session_id = %span_session_id,
                compact_kind = "manual",
                task_kind = "manual_compact_forwarder"
            )),
        );
        let tool_definitions = self.tool_registry_snapshot().definitions();
        let result = compact::force_compact(
            &mut self.messages,
            &self.settings,
            &self.llm_client,
            &tool_definitions,
            custom_instructions,
            Some(runtime_context),
            &compact_tx,
        )
        .await;
        drop(compact_tx);
        let _ = forwarder.await;

        match result {
            Ok(outcome) => {
                tracing::debug!(outcome = ?outcome, "manual compact finished");
                if compact_outcome_is_noop(&outcome) {
                    // 手动 compact 已经让 TUI 进入 working；无可压缩内容也要给一个终止事件。
                    self.send_event(RuntimeToServerEvent::warning(
                        "当前会话历史还不需要压缩".to_string(),
                    ))
                    .await;
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

pub(super) async fn persist_compact_summary_event(
    session_id: &str,
    event: &omini_domain::events::CompactSummaryFinishedEvent,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let summary = DisplaySummary {
        id: Uuid::new_v4().to_string(),
        title: "LLM Summary".to_string(),
        markdown: event.summary.clone(),
        created_at: Utc::now(),
    };
    history::persist_compact_summary_ui_message(session_id, &summary, persistence_tx).await;
}

fn compact_outcome_is_noop(outcome: &crate::runtime::compact::CompactOutcome) -> bool {
    outcome.before_tokens == outcome.after_tokens
        && outcome.before_messages == outcome.after_messages
}
