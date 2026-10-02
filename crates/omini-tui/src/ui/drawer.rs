use crate::ui::context::ViewContext;
use crate::ui::prelude::*;

pub const PERMISSION_DRAWER_MAX_HEIGHT: u16 = 18;
pub const LARGE_PERMISSION_DRAWER_MAX_HEIGHT: u16 = 50;
pub const PERMISSION_DRAWER_DIVIDER_HEIGHT: u16 = 1;
pub const USER_INPUT_NONE_LABEL: &str = "以上都不是";
pub const USER_INPUT_NONE_DESCRIPTION: &str = "可按 Tab 在备注中补充说明。";
pub const USER_INPUT_NOTE_MAX_LINES: usize = 4;
pub const USER_INPUT_NOTE_PREFIX: &str = "› ";
pub const USER_INPUT_NOTE_PLACEHOLDER: &str = "添加备注";

#[derive(Clone, Copy)]
pub struct NoteCursor {
    pub row: usize,
    pub column: usize,
}

pub struct DrawerLines {
    pub lines: Vec<Line<'static>>,
    pub note_lines: Vec<Line<'static>>,
    pub note_cursor: Option<NoteCursor>,
}

pub struct NoteRender {
    pub lines: Vec<Line<'static>>,
    pub cursor: Option<NoteCursor>,
}

#[derive(Clone, Copy)]
pub struct PermissionDrawerLinesInput<'a> {
    pub request: &'a ToolPauseRequest,
    pub tool_use: Option<&'a ToolUseBlock>,
    pub content_width: usize,
    pub project_dir: Option<&'a Path>,
    pub question_index: usize,
    pub user_input_selected: usize,
    pub current_user_input_note: &'a str,
    pub user_input_note_cursor: usize,
    pub user_input_note_mode: bool,
}

use crate::features::permissions::view::{
    build_permission_action_lines, build_permission_drawer_lines, find_tool_use,
    permission_drawer_title,
};
use crate::features::questions::view::build_input_hint;
pub fn permission_drawer_height(state: &ViewContext<'_>, area: Rect) -> u16 {
    let Some(request) = state.active_tool_pause() else {
        return 0;
    };
    if area.width == 0 || area.height == 0 {
        return 0;
    }

    let DrawerLines {
        lines, note_lines, ..
    } = build_permission_drawer_lines_for_state(state, request, area.width);
    let scroll_line_count = lines.len().saturating_sub(1);
    let note_height = note_lines.len() as u16;
    let content_height =
        permission_drawer_content_height(request, scroll_line_count, note_height, area.height);

    content_height
        .saturating_add(PERMISSION_DRAWER_DIVIDER_HEIGHT)
        .min(area.height)
}

pub fn render_permission_drawer(
    state: &mut ViewContext<'_>,
    frame: &mut ratatui::Frame,
    area: Rect,
) {
    let Some(request) = state.active_tool_pause().cloned() else {
        state.geometry.permission_drawer_area = Rect::default();
        state.geometry.permission_drawer_body_area = Rect::default();
        state.geometry.permission_drawer_content_len = 0;
        return;
    };
    if area.width == 0 || area.height == 0 {
        state.geometry.permission_drawer_area = Rect::default();
        state.geometry.permission_drawer_body_area = Rect::default();
        state.geometry.permission_drawer_content_len = 0;
        return;
    }

    let DrawerLines {
        lines,
        note_lines,
        note_cursor,
    } = build_permission_drawer_lines_for_state(state, &request, area.width);
    let fixed_header = lines.first().cloned();
    let scroll_lines: Vec<Line<'static>> = lines.into_iter().skip(1).collect();
    let note_height = note_lines.len() as u16;
    let desired_height = area.height.saturating_sub(PERMISSION_DRAWER_DIVIDER_HEIGHT);
    let body_height = desired_height.saturating_sub(5 + note_height) as usize;
    let scroll_line_count = scroll_lines.len();
    let max_scroll = scroll_line_count.saturating_sub(body_height);
    let capped_offset = state.drawer_scroll_offset.min(max_scroll);
    state.drawer_scroll_offset = capped_offset;
    let scroll_y = max_scroll.saturating_sub(capped_offset);
    state.geometry.permission_drawer_content_len = scroll_lines.len();
    let visible_lines: Vec<Line<'static>> = scroll_lines
        .into_iter()
        .skip(scroll_y)
        .take(body_height)
        .collect();

    let drawer_area = Rect {
        x: area.x,
        y: area.y.saturating_add(PERMISSION_DRAWER_DIVIDER_HEIGHT),
        width: area.width,
        height: desired_height,
    };
    let body_area = Rect {
        x: drawer_area.x + 3,
        y: drawer_area.y + 2,
        width: drawer_area
            .width
            .saturating_sub(if max_scroll > 0 { 8 } else { 6 }),
        height: body_height as u16,
    };
    state.geometry.permission_drawer_area = drawer_area;
    state.geometry.permission_drawer_body_area = body_area;

    let accent = crate::ui::theme::ACCENT;
    let divider_line = Line::from(Span::styled(
        "─".repeat(area.width.saturating_sub(1) as usize),
        Style::default().fg(accent),
    ));
    frame.render_widget(
        Paragraph::new(divider_line),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: PERMISSION_DRAWER_DIVIDER_HEIGHT,
        },
    );
    if drawer_area.height == 0 {
        return;
    }

    frame.render_widget(
        Paragraph::new("").style(
            Style::default()
                .fg(crate::ui::theme::TEXT)
                .bg(crate::ui::theme::PANEL),
        ),
        drawer_area,
    );
    let title = match &request.kind {
        ToolPauseKind::UserInput(preview) => format!(
            " 问题 {}/{}（{} 个未回答） ",
            state.dialogs.ask.user_input_question_index + 1,
            preview.questions.len(),
            state.user_input_unanswered_count()
        ),
        ToolPauseKind::Permission(_) => format!(" {} ", permission_drawer_title(&request)),
    };
    if drawer_area.height > 1 {
        let title_line = Line::from(Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        let title_area = Rect {
            x: drawer_area.x,
            y: drawer_area.y,
            width: drawer_area.width,
            height: 1,
        };
        register_selectable_lines(state, title_area, std::slice::from_ref(&title_line));
        frame.render_widget(Paragraph::new(title_line), title_area);
    }

    if drawer_area.height > 2
        && let Some(header) = fixed_header
    {
        let header_area = Rect {
            x: drawer_area.x + 3,
            y: drawer_area.y + 1,
            width: drawer_area.width.saturating_sub(6),
            height: 1,
        };
        register_selectable_lines(state, header_area, std::slice::from_ref(&header));
        frame.render_widget(Paragraph::new(header), header_area);
    }

    if body_area.width > 0 && body_area.height > 0 && body_area.y < drawer_area.bottom() {
        register_selectable_lines(state, body_area, &visible_lines);
        let paragraph = Paragraph::new(Text::from(visible_lines));
        frame.render_widget(paragraph, body_area);
    }
    if max_scroll > 0 && body_area.width > 0 && body_area.height > 0 {
        render_permission_scrollbar(frame, body_area, scroll_y, scroll_line_count);
    }

    let note_area = (!note_lines.is_empty() && drawer_area.height > 4).then_some(Rect {
        x: drawer_area.x + 3,
        y: drawer_area.y
            + drawer_area
                .height
                .saturating_sub(note_height.saturating_add(1)),
        width: drawer_area.width.saturating_sub(6),
        height: note_height,
    });
    if let Some(note_area) = note_area {
        register_selectable_lines(state, note_area, &note_lines);
        frame.render_widget(Paragraph::new(Text::from(note_lines)), note_area);
        if state.note_mode()
            && let Some(note_cursor) = note_cursor
        {
            let cursor_x = note_area.x + note_cursor.column as u16;
            let cursor_y = note_area.y + note_cursor.row as u16;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    let options = match &request.kind {
        ToolPauseKind::Permission(_) => build_permission_action_lines(state, &request),
        ToolPauseKind::UserInput(_) => build_input_hint(state),
    };
    if drawer_area.height > 2 {
        let option_lines = options.lines;
        let options_area = Rect {
            x: drawer_area.x + 3,
            y: drawer_area.y
                + drawer_area
                    .height
                    .saturating_sub(note_height.saturating_add(3)),
            width: drawer_area.width.saturating_sub(6),
            height: 2.min(drawer_area.height.saturating_sub(2)),
        };
        register_selectable_lines(state, options_area, &option_lines);
        frame.render_widget(Paragraph::new(Text::from(option_lines)), options_area);
    }
}

pub fn build_permission_drawer_lines_for_state(
    state: &ViewContext<'_>,
    request: &ToolPauseRequest,
    area_width: u16,
) -> DrawerLines {
    let preview_tool_use_id = request
        .preview_tool_use_id
        .as_deref()
        .unwrap_or(&request.tool_use_id);
    let tool_use = find_tool_use(state, preview_tool_use_id);
    let content_width = area_width.saturating_sub(6) as usize;
    build_permission_drawer_lines(PermissionDrawerLinesInput {
        request,
        tool_use: tool_use.as_ref(),
        content_width,
        project_dir: Some(state.project.status_bar.cwd.as_path()),
        question_index: state.dialogs.ask.user_input_question_index,
        user_input_selected: state.current_user_input_selected(),
        current_user_input_note: state.current_user_input_note(),
        user_input_note_cursor: state.current_user_input_note_cursor(),
        user_input_note_mode: state.note_mode(),
    })
}

pub fn permission_drawer_content_height(
    request: &ToolPauseRequest,
    scroll_line_count: usize,
    note_height: u16,
    area_height: u16,
) -> u16 {
    let available_height = area_height.saturating_sub(PERMISSION_DRAWER_DIVIDER_HEIGHT);
    if available_height == 0 {
        return 0;
    }

    let is_large_preview = matches!(
        &request.kind,
        ToolPauseKind::Permission(PermissionPreview::Bash(_))
            | ToolPauseKind::Permission(PermissionPreview::Edit(_))
            | ToolPauseKind::Permission(PermissionPreview::Write(_))
            | ToolPauseKind::Permission(PermissionPreview::Mcp(_))
    );
    let terminal_cap = ((area_height as f32) * 0.8).floor() as u16;
    let max_height = if is_large_preview {
        terminal_cap
            .min(LARGE_PERMISSION_DRAWER_MAX_HEIGHT)
            .min(available_height)
            .max(1)
    } else {
        area_height
            .saturating_sub(4)
            .clamp(7, PERMISSION_DRAWER_MAX_HEIGHT)
            .min(available_height)
    };

    (scroll_line_count as u16)
        .saturating_add(6)
        .saturating_add(note_height)
        .clamp(1, max_height)
}

pub fn render_permission_scrollbar(
    frame: &mut ratatui::Frame,
    body_area: Rect,
    scroll_y: usize,
    total_lines: usize,
) {
    let height = body_area.height as usize;
    if height == 0 || total_lines <= height {
        return;
    }

    let max_scroll = total_lines.saturating_sub(height);
    let thumb_height = (height.saturating_mul(height) / total_lines).clamp(1, height);
    let thumb_range = height.saturating_sub(thumb_height);
    let thumb_y = scroll_y
        .saturating_mul(thumb_range)
        .checked_div(max_scroll)
        .unwrap_or(0);
    let x = body_area.x + body_area.width + 1;
    let track_style = Style::default().fg(crate::ui::theme::BORDER);
    let thumb_style = Style::default().fg(crate::ui::theme::MUTED);

    for i in 0..height {
        let is_thumb = i >= thumb_y && i < thumb_y + thumb_height;
        let symbol = if is_thumb { "┃" } else { "│" };
        let style = if is_thumb { thumb_style } else { track_style };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(symbol, style))),
            Rect {
                x,
                y: body_area.y + i as u16,
                width: 1,
                height: 1,
            },
        );
    }
}

pub fn permission_option_style(selected: bool) -> Style {
    let style = Style::default();
    if selected {
        style
            .fg(crate::ui::theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

pub fn set_note_line(
    drawer: &mut DrawerLines,
    note: &str,
    cursor: usize,
    editing: bool,
    reserve_empty: bool,
    content_width: usize,
) {
    if !reserve_empty && !editing && note.is_empty() {
        return;
    }

    if note.is_empty() && !editing {
        drawer.note_lines = vec![Line::from("")];
        drawer.note_cursor = None;
    } else {
        let NoteRender { lines, cursor } =
            user_input_note_lines(note, cursor, editing, content_width);
        drawer.note_lines = lines;
        drawer.note_cursor = cursor;
    }
}

pub fn wrap_preserving_display_width(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    let mut lines = Vec::new();

    for source_line in text.split('\n') {
        if source_line.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_width = 0;
        for ch in source_line.chars() {
            let char_width = ch.width().unwrap_or(0);
            if current_width > 0 && current_width + char_width > max_width {
                lines.push(current);
                current = String::new();
                current_width = 0;
            }
            current.push(ch);
            current_width += char_width;
        }
        lines.push(current);
    }

    lines
}

pub fn user_input_note_lines(
    note: &str,
    cursor_char: usize,
    editing: bool,
    content_width: usize,
) -> NoteRender {
    let marker_style = Style::default().fg(crate::ui::theme::MUTED);
    let value_style = if note.is_empty() {
        Style::default().fg(crate::ui::theme::MUTED)
    } else {
        Style::default().fg(crate::ui::theme::TEXT)
    };
    let prefix_width = USER_INPUT_NOTE_PREFIX.width();

    if note.is_empty() {
        return NoteRender {
            lines: vec![Line::from(vec![
                Span::styled(USER_INPUT_NOTE_PREFIX, marker_style),
                Span::styled(USER_INPUT_NOTE_PLACEHOLDER.to_string(), value_style),
            ])],
            cursor: editing.then_some(NoteCursor {
                row: 0,
                column: prefix_width,
            }),
        };
    }

    let value_width = content_width.saturating_sub(prefix_width).max(1);
    let (value_lines, mut cursor) = wrap_note_value(note, cursor_char, value_width);
    cursor.column += prefix_width;
    let line_count = value_lines.len();
    let start = if line_count > USER_INPUT_NOTE_MAX_LINES {
        if editing {
            cursor
                .row
                .saturating_add(1)
                .saturating_sub(USER_INPUT_NOTE_MAX_LINES)
        } else {
            line_count.saturating_sub(USER_INPUT_NOTE_MAX_LINES)
        }
    } else {
        0
    };
    let end = (start + USER_INPUT_NOTE_MAX_LINES).min(line_count);
    let cursor = editing.then_some(cursor).and_then(|cursor| {
        (cursor.row >= start && cursor.row < end).then_some(NoteCursor {
            row: cursor.row - start,
            column: cursor.column,
        })
    });
    let lines = value_lines
        .into_iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(idx, value)| {
            let prefix = if idx == 0 {
                USER_INPUT_NOTE_PREFIX.to_string()
            } else {
                " ".repeat(prefix_width)
            };
            let prefix_style = if idx == 0 {
                marker_style
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled(value, value_style),
            ])
        })
        .collect();

    NoteRender { lines, cursor }
}

pub fn wrap_note_value(
    note: &str,
    cursor_char: usize,
    max_width: usize,
) -> (Vec<String>, NoteCursor) {
    let max_width = max_width.max(1);
    let cursor_char = cursor_char.min(note.chars().count());
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    let mut char_idx = 0;
    let mut cursor = None;

    for ch in note.chars() {
        if ch == '\n' {
            if char_idx == cursor_char && cursor.is_none() {
                cursor = Some(NoteCursor {
                    row: lines.len(),
                    column: current_width,
                });
            }
            lines.push(current);
            current = String::new();
            current_width = 0;
            char_idx += 1;
            if char_idx == cursor_char && cursor.is_none() {
                cursor = Some(NoteCursor {
                    row: lines.len(),
                    column: 0,
                });
            }
            continue;
        }

        let char_width = ch.width().unwrap_or(0);
        if current_width > 0 && current_width + char_width > max_width {
            lines.push(current);
            current = String::new();
            current_width = 0;
        }
        if char_idx == cursor_char && cursor.is_none() {
            cursor = Some(NoteCursor {
                row: lines.len(),
                column: current_width,
            });
        }
        current.push(ch);
        current_width += char_width;
        char_idx += 1;
    }

    if cursor.is_none() {
        cursor = Some(NoteCursor {
            row: lines.len(),
            column: current_width,
        });
    }
    lines.push(current);

    (lines, cursor.expect("note cursor should be set"))
}
