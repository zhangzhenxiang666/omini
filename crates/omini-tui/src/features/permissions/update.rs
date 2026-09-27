use crate::app::effect::Effects;
use crate::app::event::ToolPauseKind;
use crate::app::state::AppState;
use crate::client::ClientRequest;
use crate::features::agents::actions as input;
use crossterm::event::KeyCode;

pub fn handle_tool_pause_key(state: &mut AppState, code: KeyCode, request_tx: &mut Effects) {
    let Some(active_pause) = state.active_tool_pause().cloned() else {
        return;
    };
    let user_input_option_max = match &active_pause.kind {
        ToolPauseKind::UserInput(preview) => preview
            .questions
            .get(state.dialogs.ask.user_input_question_index)
            .map(|question| question.options.len())
            .unwrap_or(0),
        ToolPauseKind::Permission(_) => 1,
    };
    let is_permission_pause = matches!(&active_pause.kind, ToolPauseKind::Permission(_));
    let is_user_input_pause = matches!(&active_pause.kind, ToolPauseKind::UserInput(_));

    if state.note_mode() {
        match code {
            KeyCode::Tab | KeyCode::Esc => state.set_note_mode(false),
            KeyCode::Enter if is_permission_pause => {
                state.dialogs.permission.permission_selected = 1;
                input::resolve_active_tool_pause(state, request_tx);
            }
            KeyCode::Enter => {
                state.mark_current_user_input_answered();
                if state.user_input_unanswered_count() == 0 {
                    input::resolve_active_tool_pause(state, request_tx);
                } else {
                    state.move_to_next_unanswered_user_input();
                }
            }
            KeyCode::Backspace => state.delete_note_before(),
            KeyCode::Delete => state.delete_note_after(),
            // note 模式是纯文本输入,`j` / `k` 不再被当成方向键别名,
            // 必须能作为普通字符插入;方向键导航仍由 `Up` / `Down` 负责。
            // 修复 Bug 1:之前 `KeyCode::Char('k')` / `KeyCode::Char('j')`
            // 在 `if is_user_input_pause` 守卫下吞掉了 ask_user note 模式
            // 中的 `j` / `k` 字符。
            KeyCode::Up if is_user_input_pause => state.permission_select_prev(),
            KeyCode::Down if is_user_input_pause => {
                state.permission_select_next_with_max(user_input_option_max);
            }
            KeyCode::Char(c) => state.insert_note_char(c),
            KeyCode::Left => state.note_cursor_left(),
            KeyCode::Right => state.note_cursor_right(),
            KeyCode::Home => state.note_cursor_home(),
            KeyCode::End => state.note_cursor_end(),
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Up | KeyCode::Char('k') => state.permission_select_prev(),
        KeyCode::Down | KeyCode::Char('j') => {
            state.permission_select_next_with_max(user_input_option_max);
        }
        KeyCode::Left | KeyCode::Char('h') if is_user_input_pause => {
            state.user_input_question_prev();
        }
        KeyCode::Right | KeyCode::Char('l') if is_user_input_pause => {
            state.user_input_question_next();
        }
        KeyCode::PageUp => {
            let page = 1.max(state.geometry.permission_drawer_body_area.height as usize / 2);
            state.permission_scroll_up(page);
        }
        KeyCode::PageDown => {
            let page = 1.max(state.geometry.permission_drawer_body_area.height as usize / 2);
            state.permission_scroll_down(page);
        }
        KeyCode::Tab if is_permission_pause => {
            state.dialogs.permission.permission_selected = 1;
            state.set_note_mode(true);
            state.note_cursor_end();
        }
        KeyCode::Tab if is_user_input_pause => {
            state.set_note_mode(true);
            state.note_cursor_end();
        }
        KeyCode::Char('y') | KeyCode::Char('Y') if is_permission_pause => {
            state.dialogs.permission.permission_selected = 0;
            input::resolve_active_tool_pause(state, request_tx);
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') if is_permission_pause => {
            state.dialogs.permission.permission_selected = 1;
            input::resolve_active_tool_pause(state, request_tx);
        }
        KeyCode::Esc => {
            state.dialogs.permission.permission_selected = 1;
            let _ = request_tx.send(ClientRequest::ToolPauseResolve {
                tool_use_id: active_pause.tool_use_id.clone(),
                response: omini_protocol::ToolPauseResponse::Cancelled,
            });
            let removed_active = state.remove_tool_pause(&active_pause.tool_use_id);
            state.finish_tool_pause_removal(removed_active);
        }
        KeyCode::Enter => {
            if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) {
                state.mark_current_user_input_answered();
                if state.user_input_unanswered_count() == 0 {
                    input::resolve_active_tool_pause(state, request_tx);
                } else {
                    state.move_to_next_unanswered_user_input();
                }
            } else {
                input::resolve_active_tool_pause(state, request_tx);
            }
        }
        _ => {}
    }
}
