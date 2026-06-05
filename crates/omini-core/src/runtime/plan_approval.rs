use super::service::{AgentRuntime, RunStart};
use super::*;

impl AgentRuntime {
    pub(super) async fn resolve_plan_approval(
        &mut self,
        plan_id: &str,
        action: PlanApprovalAction,
    ) {
        match action {
            PlanApprovalAction::ContinueDiscussing => {
                self.set_active_profile(ActiveProfile::Plan);
                self.send_event(RuntimeToServerEvent::ActiveProfileChanged(
                    self.active_profile(),
                ))
                .await;
                self.send_plan_approval_resolved(plan_id, action).await;
            }
            PlanApprovalAction::Approve { profile } => {
                let plan_message = Message::from_user_text(plan::approval_message());
                self.send_plan_approval_resolved(plan_id, action).await;
                self.set_active_profile(profile.active_profile());
                self.send_event(RuntimeToServerEvent::ActiveProfileChanged(
                    self.active_profile(),
                ))
                .await;
                self.messages.push(plan_message.clone());
                self.send_event(RuntimeToServerEvent::UserMessageInjected(
                    HistoryItem::Message(plan_message),
                ))
                .await;
                self.process_run(RunStart::UserMessage).await;
            }
            PlanApprovalAction::ApproveAndCompact { profile } => {
                let path = self.plan_path(plan_id);
                let plan_content = match std::fs::read_to_string(&path) {
                    Ok(content) => content,
                    Err(e) => {
                        self.send_event(RuntimeToServerEvent::error(format!(
                            "无法压缩规划上下文，读取计划失败 {}: {e}",
                            path.display()
                        )))
                        .await;
                        return;
                    }
                };
                // 读到计划后才关闭所有客户端的审批抽屉，避免失败时丢掉可重试的 pending 计划。
                self.send_plan_approval_resolved(plan_id, action).await;
                let plan_message = Message::from_user_text(plan::compacted_context(&plan_content));
                self.session_id = None;
                self.session_dir = None;
                self.messages = vec![plan_message.clone()];
                self.set_active_profile(profile.active_profile());
                self.create_session(Some(HistoryItem::Message(plan_message.clone())))
                    .await;
                self.persist_compacted_plan_initial_message(plan_message)
                    .await;
                self.send_event(RuntimeToServerEvent::ActiveProfileChanged(
                    self.active_profile(),
                ))
                .await;
                self.process_run(RunStart::Continue).await;
            }
        }
    }

    async fn send_plan_approval_resolved(&self, plan_id: &str, action: PlanApprovalAction) {
        self.send_event(RuntimeToServerEvent::PlanApprovalResolved {
            plan_id: plan_id.to_string(),
            action,
        })
        .await;
    }

    fn plan_path(&self, plan_id: &str) -> std::path::PathBuf {
        plan::path(&self.project, plan_id)
    }

    async fn persist_compacted_plan_initial_message(&self, message: Message) {
        let Some(session_id) = self.session_id.as_deref() else {
            return;
        };
        let Some(session_dir) = self.session_dir.as_ref() else {
            return;
        };

        let blocks_dir = session_dir.path().join("blocks");
        history::persist_one(
            session_dir,
            session_id,
            &blocks_dir,
            message,
            &self.persistence_tx,
        )
        .await;
    }

    pub(super) async fn persist_latest_proposed_plan(
        &self,
    ) -> Result<Option<SubmittedPlan>, String> {
        let submitted =
            plan::persist_latest(&self.project, self.active_profile(), &self.messages).await?;
        if let Some(plan) = submitted.as_ref()
            && let Some(session_id) = self.session_id.as_deref()
        {
            history::persist_plan_ui_message(session_id, plan, &self.persistence_tx).await;
        }
        Ok(submitted)
    }
}
