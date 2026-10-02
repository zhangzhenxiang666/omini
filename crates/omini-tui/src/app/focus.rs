use crate::app::state::{AppState, InteractionStep};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    StopConfirmation,
    Page,
    Plan,
    Pause,
    Help,
    Model,
    SessionSelector,
    Composer,
}
/// 停止确认独占输入；其余页面、计划和暂停请求保持原有优先级。
pub fn current(state: &AppState) -> Focus {
    if state.dialogs.stop_confirmation.is_some() {
        Focus::StopConfirmation
    } else if matches!(
        state.dialogs.interaction_step,
        Some(InteractionStep::Thread { .. } | InteractionStep::Agents(_))
    ) {
        Focus::Page
    } else if state.dialogs.plan.plan_approval.is_some() {
        Focus::Plan
    } else if state.active_tool_pause().is_some() {
        Focus::Pause
    } else if state.dialogs.help_drawer.is_some() {
        Focus::Help
    } else if state.dialogs.interaction_step.is_some() {
        Focus::Model
    } else if state.sessions.session_selector_focused {
        Focus::SessionSelector
    } else {
        Focus::Composer
    }
}
