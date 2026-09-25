use crate::clipboard::copy_to_clipboard;
use crate::selection::{
    selected_text, selection_point_from_mouse, update_text_selection_from_mouse,
};
use crate::state::{TextSelection, UiState};
use crate::types::events::{PermissionPreview, ToolPauseKind};
use crossterm::event::{MouseButton, MouseEventKind};

pub(super) fn handle_mouse_event(state: &mut UiState, kind: MouseEventKind, row: u16, column: u16) {
    if state.is_selecting_text {
        match kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                update_text_selection_from_mouse(state, row, column);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                update_text_selection_from_mouse(state, row, column);
                state.is_selecting_text = false;
                if let Some(text) = selected_text(state) {
                    copy_to_clipboard(&text);
                }
                state.text_selection = None;
            }
            _ => {}
        }
        return;
    }

    if active_permission_drawer_captures_scroll(state) {
        match kind {
            MouseEventKind::ScrollUp => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_up(state.scroll_step);
                return;
            }
            MouseEventKind::ScrollDown => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_down(state.scroll_step);
                return;
            }
            _ => {}
        }
    }

    if state.active_tool_pause().is_some() {
        let drawer = state.permission_drawer_area;
        let in_drawer = row >= drawer.top()
            && row < drawer.bottom()
            && column >= drawer.left()
            && column < drawer.right();
        let in_action_row =
            row == drawer.bottom().saturating_sub(2) || row == drawer.bottom().saturating_sub(1);
        if let MouseEventKind::Down(MouseButton::Left) = kind
            && in_drawer
            && in_action_row
        {
            state.permission_selected = if row == drawer.bottom().saturating_sub(2) {
                0
            } else {
                1
            };
            return;
        }
    }

    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(point) = selection_point_from_mouse(state, row, column) {
                state.text_selection = Some(TextSelection {
                    start: point,
                    end: point,
                });
                state.is_selecting_text = true;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            update_text_selection_from_mouse(state, row, column);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            update_text_selection_from_mouse(state, row, column);
            state.is_selecting_text = false;
            if let Some(text) = selected_text(state) {
                copy_to_clipboard(&text);
            }
            state.text_selection = None;
        }
        MouseEventKind::ScrollUp => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_up(state.scroll_step);
        }
        MouseEventKind::ScrollDown => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_down(state.scroll_step);
        }
        _ => {}
    }
}

fn active_permission_drawer_captures_scroll(state: &UiState) -> bool {
    let Some(request) = state.active_tool_pause() else {
        return false;
    };
    let is_large_file_preview = matches!(
        &request.kind,
        ToolPauseKind::Permission(PermissionPreview::Bash(_))
            | ToolPauseKind::Permission(PermissionPreview::Edit(_))
            | ToolPauseKind::Permission(PermissionPreview::Write(_))
            | ToolPauseKind::Permission(PermissionPreview::Mcp(_))
    );
    is_large_file_preview
        && state.permission_drawer_body_area.height > 0
        && state.permission_drawer_content_len > state.permission_drawer_body_area.height as usize
}
