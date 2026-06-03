use super::service::AgentRuntime;
use super::usage::record_total_usage_and_notify;
use super::*;

impl AgentRuntime {
    pub(super) async fn compact_context(&mut self, custom_instructions: Option<&str>) {
        if let Err(error) = self
            .force_compact_current_session(custom_instructions)
            .await
        {
            self.send_event(RuntimeToUiEvent::Notification(Notification::warning(error)))
                .await;
        }
    }

    pub(crate) async fn force_compact_current_session(
        &mut self,
        custom_instructions: Option<&str>,
    ) -> Result<(), String> {
        if self.messages.is_empty() {
            return Err("没有可压缩的会话历史".to_string());
        }
        if self.session_id.is_none() || self.session_dir.is_none() {
            return Err("当前没有已创建的会话，无法压缩历史".to_string());
        }

        let subagent_registry = self.capabilities.subagent_registry();
        let skill_registry = self.capabilities.skill_registry();
        let runtime_context = Arc::new(ToolRuntimeContext {
            session_id: self
                .session_id
                .clone()
                .expect("session id checked before compact"),
            session_type: "main".to_string(),
            agent_label: None,
            session_dir: self
                .session_dir
                .clone()
                .expect("session dir checked before compact"),
            subagent_registry,
            skill_registry,
            subagent_runner: Some(Arc::clone(&self.subagent_runner)),
            project: self.project.clone(),
        });
        let (compact_tx, mut compact_rx) = mpsc::channel(16);
        let event_tx = self.event_tx.clone();
        let persistence_tx = self.persistence_tx.clone();
        let usage_state = Arc::clone(&self.session_usage);
        let session_id = self
            .session_id
            .clone()
            .expect("session id checked before compact");
        let forwarder = tokio::spawn(async move {
            while let Some(event) = compact_rx.recv().await {
                match event {
                    EngineToRuntimeEvent::CompactShrinkStarted(_)
                    | EngineToRuntimeEvent::CompactShrinkFinished(_)
                    | EngineToRuntimeEvent::CompactShrinkFailed(_) => {
                        // TODO(compact): 收缩操作暂不通知 UI，后续再决定是否记录内部状态。
                    }
                    EngineToRuntimeEvent::CompactSummaryStarted(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryStarted(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryDelta(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryDelta(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFinished(event) => {
                        persist_compact_summary_event(&session_id, &event, &persistence_tx).await;
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryFinished(event))
                            .await;
                    }
                    EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::CompactSummaryFailed(event))
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
                    _ => {}
                }
            }
        });
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
                if compact_outcome_is_noop(&outcome) {
                    // 手动 compact 已经让 TUI 进入 working；无可压缩内容也要给一个终止事件。
                    self.send_event(RuntimeToUiEvent::warning(
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
    event: &crate::types::events::CompactSummaryFinishedEvent,
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
