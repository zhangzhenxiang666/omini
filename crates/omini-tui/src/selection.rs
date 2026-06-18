use crate::state::{SelectionPoint, UiState};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) const SELECTION_HIGHLIGHT: Style = Style::new()
    .fg(Color::Rgb(40, 44, 52))
    .bg(Color::Rgb(180, 210, 255))
    .add_modifier(Modifier::BOLD);

pub fn selection_point_from_mouse(
    state: &UiState,
    row: u16,
    column: u16,
) -> Option<SelectionPoint> {
    let line = state.selectable_screen_lines.iter().find(|line| {
        row == line.row && column >= line.col && column < line.col.saturating_add(line.width)
    })?;
    let text_width = UnicodeWidthStr::width(line.text.as_str());
    let col = column.saturating_sub(line.col) as usize;
    Some(SelectionPoint {
        row: row as usize,
        col: col.min(text_width),
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
    for line in state.selectable_screen_lines.iter().filter(|line| {
        let row = line.row as usize;
        row >= start.row && row <= end.row
    }) {
        let row = line.row as usize;
        let start_col = if row == start.row { start.col } else { 0 };
        let end_col = if row == end.row {
            end.col.saturating_add(1)
        } else {
            UnicodeWidthStr::width(line.text.as_str())
        };
        lines.push(
            slice_display_cols(&line.text, start_col, end_col)
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

pub(super) fn apply_selection_overlay(state: &UiState, buf: &mut ratatui::buffer::Buffer) {
    let Some((start, end)) = normalized_selection(state) else {
        return;
    };
    if start == end {
        return;
    }

    for line in &state.selectable_screen_lines {
        let row = line.row as usize;
        if row < start.row || row > end.row {
            continue;
        }

        let start_col = if row == start.row { start.col } else { 0 };
        let end_col = if row == end.row {
            end.col.saturating_add(1)
        } else {
            UnicodeWidthStr::width(line.text.as_str())
        };
        if start_col >= end_col {
            continue;
        }

        let mut col = 0usize;
        for ch in line.text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width == 0 {
                continue;
            }
            let next_col = col + width;
            if next_col > start_col && col < end_col {
                let buf_x = line.col + col as u16;
                let buf_y = line.row;
                if let Some(cell) = buf.cell_mut((buf_x, buf_y)) {
                    cell.set_style(SELECTION_HIGHLIGHT);
                }
            }
            col = next_col;
        }
    }
}

fn normalized_selection(state: &UiState) -> Option<(SelectionPoint, SelectionPoint)> {
    let selection = state.text_selection.as_ref()?;
    if selection.start <= selection.end {
        Some((selection.start, selection.end))
    } else {
        Some((selection.end, selection.start))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TextSelection, UiState};

    fn state_with_lines(lines: &[(u16, &str)]) -> UiState {
        let mut state = UiState::new();
        for (row, text) in lines {
            state.register_selectable_screen_line(*row, 0, 80, (*text).to_string());
        }
        state
    }

    #[test]
    fn selects_single_input_line_text() {
        let mut state = state_with_lines(&[(10, "❯ hello world")]);
        state.text_selection = Some(TextSelection {
            start: SelectionPoint { row: 10, col: 2 },
            end: SelectionPoint { row: 10, col: 6 },
        });

        assert_eq!(selected_text(&state), Some("hello".to_string()));
    }

    #[test]
    fn selects_multiline_input_text() {
        let mut state = state_with_lines(&[(10, "❯ hello"), (11, "  world")]);
        state.text_selection = Some(TextSelection {
            start: SelectionPoint { row: 10, col: 2 },
            end: SelectionPoint { row: 11, col: 6 },
        });

        assert_eq!(selected_text(&state), Some("hello\n  world".to_string()));
    }

    #[test]
    fn selects_across_registered_screen_regions() {
        let mut state = state_with_lines(&[(2, "assistant line"), (3, ""), (10, "❯ prompt")]);
        state.text_selection = Some(TextSelection {
            start: SelectionPoint { row: 2, col: 0 },
            end: SelectionPoint { row: 10, col: 7 },
        });

        assert_eq!(
            selected_text(&state),
            Some("assistant line\n\n❯ prompt".to_string())
        );
    }

    #[test]
    fn selection_uses_display_columns_for_wide_chars() {
        let mut state = state_with_lines(&[(5, "❯ 你好abc")]);
        state.text_selection = Some(TextSelection {
            start: SelectionPoint { row: 5, col: 2 },
            end: SelectionPoint { row: 5, col: 5 },
        });

        assert_eq!(selected_text(&state), Some("你好".to_string()));
    }

    #[test]
    fn mouse_outside_selectable_lines_is_ignored() {
        let mut state = state_with_lines(&[(4, "visible")]);
        state.text_selection = Some(TextSelection {
            start: SelectionPoint { row: 4, col: 0 },
            end: SelectionPoint { row: 4, col: 0 },
        });

        update_text_selection_from_mouse(&mut state, 7, 10);

        assert_eq!(
            state.text_selection.as_ref().map(|selection| selection.end),
            Some(SelectionPoint { row: 4, col: 0 })
        );
        assert_eq!(selection_point_from_mouse(&state, 7, 10), None);
    }
}
