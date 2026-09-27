use crate::app::effect::Effects;
use crate::app::event::{PermissionPreview, ToolPauseKind};
use crate::app::state::{AppState, TextSelection};
use crate::ui::selection::{
    selected_text, selection_point_from_mouse, update_text_selection_from_mouse,
};
use crossterm::event::{MouseButton, MouseEventKind};

pub fn handle_mouse_event(
    state: &mut AppState,
    kind: MouseEventKind,
    row: u16,
    column: u16,
    effects: &mut Effects,
) {
    if state.selection.is_selecting_text {
        match kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                update_text_selection_from_mouse(state, row, column);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                update_text_selection_from_mouse(state, row, column);
                state.selection.is_selecting_text = false;
                if let Some(text) = selected_text(state) {
                    effects.clipboard.push(text);
                }
                state.selection.text_selection = None;
            }
            _ => {}
        }
        return;
    }

    if active_permission_drawer_captures_scroll(state) {
        match kind {
            MouseEventKind::ScrollUp => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_up(state.selection.scroll_step);
                return;
            }
            MouseEventKind::ScrollDown => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_down(state.selection.scroll_step);
                return;
            }
            _ => {}
        }
    }

    if state.active_tool_pause().is_some() {
        let drawer = state.geometry.permission_drawer_area;
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
            state.dialogs.permission.permission_selected =
                if row == drawer.bottom().saturating_sub(2) {
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
                state.selection.text_selection = Some(TextSelection {
                    start: point,
                    end: point,
                });
                state.selection.is_selecting_text = true;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            update_text_selection_from_mouse(state, row, column);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            update_text_selection_from_mouse(state, row, column);
            state.selection.is_selecting_text = false;
            if let Some(text) = selected_text(state) {
                effects.clipboard.push(text);
            }
            state.selection.text_selection = None;
        }
        MouseEventKind::ScrollUp => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_up(state.selection.scroll_step);
        }
        MouseEventKind::ScrollDown => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_down(state.selection.scroll_step);
        }
        _ => {}
    }
}

fn active_permission_drawer_captures_scroll(state: &AppState) -> bool {
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
        && state.geometry.permission_drawer_body_area.height > 0
        && state.geometry.permission_drawer_content_len
            > state.geometry.permission_drawer_body_area.height as usize
}
