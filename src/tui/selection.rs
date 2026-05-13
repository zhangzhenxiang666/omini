use crate::tui::state::{SelectionPoint, UiState};
use ratatui::layout::Rect;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn mouse_in_area(row: u16, column: u16, area: Rect) -> bool {
    row >= area.top() && row < area.bottom() && column >= area.left() && column < area.right()
}

pub fn selection_point_from_mouse(
    state: &UiState,
    row: u16,
    column: u16,
) -> Option<SelectionPoint> {
    if state.selectable_message_lines.is_empty() || !mouse_in_area(row, column, state.messages_area)
    {
        return None;
    }

    let viewport_row = row.saturating_sub(state.messages_area.top()) as usize;
    let content_row = state.message_scroll_y.saturating_add(viewport_row);
    if content_row >= state.selectable_message_lines.len() {
        return None;
    }

    let col = column.saturating_sub(state.messages_area.left()) as usize;
    Some(SelectionPoint {
        row: content_row,
        col,
    })
}

pub fn update_text_selection_from_mouse(state: &mut UiState, row: u16, column: u16) {
    let Some(point) = selection_point_from_mouse(state, row, column) else {
        return;
    };
    if let Some(selection) = &mut state.text_selection {
        selection.end = point;
    }
}

pub fn selected_text(state: &UiState) -> Option<String> {
    let selection = state.text_selection.as_ref()?;
    if selection.start == selection.end {
        return None;
    }

    let (start, end) = if selection.start <= selection.end {
        (selection.start, selection.end)
    } else {
        (selection.end, selection.start)
    };

    let mut lines = Vec::new();
    for row in start.row..=end.row {
        let line = state.selectable_message_lines.get(row)?;
        let start_col = if row == start.row { start.col } else { 0 };
        let end_col = if row == end.row {
            end.col.saturating_add(1)
        } else {
            UnicodeWidthStr::width(line.as_str())
        };
        lines.push(
            slice_display_cols(line, start_col, end_col)
                .trim_end()
                .to_string(),
        );
    }

    let text = lines.join("\n");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn slice_display_cols(text: &str, start_col: usize, end_col: usize) -> String {
    let mut result = String::new();
    let mut col = 0;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        let next_col = col + width;
        if next_col > start_col && col < end_col {
            result.push(ch);
        }
        col = next_col;
    }
    result
}
