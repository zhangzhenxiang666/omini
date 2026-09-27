use crate::app::effect::Effects;
use crate::app::event::{PlanApprovalAction, PlanExecutionProfile};
use crate::app::state::AppState;
use crate::client::ClientRequest;
use crate::client::input as protocol;
use crossterm::event::KeyCode;

pub fn handle_plan_approval_key(state: &mut AppState, code: KeyCode, request_tx: &mut Effects) {
    if state.dialogs.plan.plan_approval.is_none() {
        return;
    }
    match code {
        KeyCode::Up | KeyCode::Char('k') => {
            state.dialogs.plan.plan_approval_selected =
                state.dialogs.plan.plan_approval_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.dialogs.plan.plan_approval_selected =
                (state.dialogs.plan.plan_approval_selected + 1).min(2);
        }
        KeyCode::Char('1') => {
            state.dialogs.plan.plan_approval_selected = 0;
            submit_plan_approval(state, request_tx);
        }
        KeyCode::Char('2') => {
            state.dialogs.plan.plan_approval_selected = 1;
            submit_plan_approval(state, request_tx);
        }
        KeyCode::Char('3') => {
            state.dialogs.plan.plan_approval_selected = 2;
            submit_plan_approval(state, request_tx);
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            state.dialogs.plan.plan_approval_auto = !state.dialogs.plan.plan_approval_auto;
        }
        KeyCode::Esc => {
            state.dialogs.plan.plan_approval = None;
            state.dialogs.plan.plan_approval_selected = 0;
            state.dialogs.plan.plan_approval_auto = false;
            let _ = request_tx.send(ClientRequest::PlanResolve {
                action: omini_protocol::PlanApprovalAction::ContinueDiscussing,
            });
        }
        KeyCode::Enter => {
            submit_plan_approval(state, request_tx);
        }
        _ => {}
    }
}

pub fn submit_plan_approval(state: &mut AppState, request_tx: &mut Effects) {
    let Some(_) = state.dialogs.plan.plan_approval.take() else {
        return;
    };
    let profile = if state.dialogs.plan.plan_approval_auto {
        PlanExecutionProfile::Auto
    } else {
        PlanExecutionProfile::Main
    };
    let action = match state.dialogs.plan.plan_approval_selected.min(2) {
        0 => PlanApprovalAction::Approve { profile },
        1 => PlanApprovalAction::ApproveInNewThread { profile },
        _ => PlanApprovalAction::ContinueDiscussing,
    };
    state.dialogs.plan.plan_approval_selected = 0;
    state.dialogs.plan.plan_approval_auto = false;
    let _ = request_tx.send(ClientRequest::PlanResolve {
        action: protocol::plan_approval_action_from_internal(action),
    });
}
