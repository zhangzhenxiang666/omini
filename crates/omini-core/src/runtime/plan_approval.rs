use super::service::{AgentRuntime, RunStart};
use super::*;
use omini_domain::display::HistoryItem;

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
                self.send_event(RuntimeToServerEvent::UserMessageInjected {
                    item: HistoryItem::Message(plan_message),
                    client_echo_id: None,
                })
                .await;
                self.process_run(RunStart::UserMessage).await;
            }
            // Server 路由层在收到此 action 时已自行 fork 新 session 并广播
            // SessionSwitched；core 这里只关闭原 thread 的审批抽屉，不改状态。
            PlanApprovalAction::ApproveInNewSession { .. } => {
                self.send_plan_approval_resolved(plan_id, action).await;
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

    pub(super) async fn persist_latest_proposed_plan(
        &self,
    ) -> Result<Option<SubmittedPlan>, String> {
        let submitted =
            plan::persist_latest(&self.project, self.active_profile(), &self.messages).await?;
        if let Some(plan) = submitted.as_ref() {
            history::persist_plan_ui_message(
                &self.thread_id,
                plan,
                &format!("{}/{}", self.settings.active_provider, self.settings.model),
                &self.persistence_tx,
            )
            .await;
        }
        Ok(submitted)
    }
}
