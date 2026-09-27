use crate::app::state::{AppState, InteractionStep};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Page,
    Plan,
    Pause,
    Help,
    Model,
    Composer,
}
/// 页面、计划和暂停请求拥有稳定优先级；关闭后恢复原来的局部交互。
pub fn current(state: &AppState) -> Focus {
    if matches!(
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
    } else {
        Focus::Composer
    }
}
