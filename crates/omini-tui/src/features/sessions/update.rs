use crate::app::state::AppState;
use crossterm::event::KeyCode;

/// 会话列表独占按键；方向键只移动高亮项，Enter 才切换视图。
pub fn handle_selector_key(state: &mut AppState, code: KeyCode) {
    match code {
        KeyCode::Up if state.sessions.session_selection_index == 0 => {
            state.sessions.session_selector_focused = false;
        }
        KeyCode::Up => {
            state.sessions.session_selection_index -= 1;
        }
        KeyCode::Down if state.sessions.session_selection_index + 1 < state.session_count() => {
            state.sessions.session_selection_index += 1;
        }
        KeyCode::Enter => {
            activate_selection(state);
            state.sessions.session_selector_focused = false;
        }
        KeyCode::Esc => state.sessions.session_selector_focused = false,
        _ => {}
    }
}

/// 确认高亮项后切换视图，并回收已离开的终态任务。
fn activate_selection(state: &mut AppState) {
    state.sessions.active_session_task_id = state
        .sessions
        .session_selection_index
        .checked_sub(1)
        .and_then(|index| state.sessions.subagent_order.get(index))
        .cloned();
    state.prune_terminal_tasks();
}
