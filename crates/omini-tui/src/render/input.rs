use crate::selection::{highlighted_line, selected_cols_for_screen_line};
use crate::state::UiState;
use crate::types::display::UserDraft;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub(super) fn render_input(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    let bg = Style::default().bg(Color::Rgb(65, 69, 76));

    let bg_widget = Paragraph::new(Line::from("")).style(bg);
    frame.render_widget(bg_widget, area);
    let visible_line_count = state.input_visible_line_count();

    let drawer_len = queued_drawer_inputs(state).len();
    let queued_height = if drawer_len == 0 {
        0
    } else {
        drawer_len.min(4) as u16 + 2
    };
    let input_area = if queued_height > 0 {
        let chunks = Layout::vertical([
            Constraint::Length(queued_height),
            Constraint::Length(2 + visible_line_count as u16),
        ])
        .split(area);
        render_queued_user_inputs(state, frame, chunks[0]);
        chunks[1]
    } else {
        area
    };

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(visible_line_count as u16),
        Constraint::Length(1),
    ])
    .split(input_area);
    let input_body = chunks[1];

    let line_bg = Paragraph::new(Line::from(Span::styled(
        " ".repeat(area.width as usize),
        bg,
    )))
    .style(bg);
    frame.render_widget(line_bg, input_body);

    let prefix_style = Style::default().fg(Color::Rgb(0xab, 0xab, 0xab));
    let cmd_color = Style::default().fg(Color::Rgb(0x42, 0xd9, 0xe8));
    let placeholder_style = Style::default().fg(Color::DarkGray);

    let mut lines = if state.input.is_empty() {
        vec![Line::from(vec![
            Span::styled("\u{276f} ", prefix_style),
            Span::styled("输入消息...", placeholder_style),
        ])]
    } else {
        input_lines(state, prefix_style, cmd_color)
    };
    if !state.input.is_empty() {
        register_input_lines(state, input_body, &lines);
        apply_selection_highlight(state, input_body, &mut lines);
    }
    let paragraph = Paragraph::new(lines).style(bg);
    frame.render_widget(paragraph, input_body);

    let (cursor_x, cursor_y) = if state.input.is_empty() {
        (input_body.x + 2, input_body.y)
    } else {
        let (line_idx, col) = state.input_cursor_line_col().unwrap_or((0, 0));
        let visible_line = line_idx.saturating_sub(state.input_scroll_line);
        let x_offset = state.input_visual_line_prefix_width(line_idx) as u16;
        (
            input_body.x + x_offset + col_width(state, line_idx, col) as u16,
            input_body.y + visible_line as u16,
        )
    };
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn input_lines(state: &UiState, prefix_style: Style, cmd_color: Style) -> Vec<Line<'static>> {
    let command_end_char = command_highlight_end(state);
    let args_hint = command_args_hint(state);
    let input_char_len = state.input.chars().count();
    let hint_style = Style::default().fg(Color::DarkGray);
    state
        .input_line_bounds()
        .into_iter()
        .enumerate()
        .skip(state.input_scroll_line)
        .take(state.input_visible_line_count())
        .map(|(idx, (start, end))| {
            let mut spans = Vec::new();
            if idx == 0 {
                spans.push(Span::styled("\u{276f} ", prefix_style));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.extend(input_spans(state, start, end, command_end_char, cmd_color));
            if end == input_char_len
                && let Some(hint) = args_hint
            {
                spans.push(Span::styled(hint.to_string(), hint_style));
            }
            Line::from(spans)
        })
        .collect()
}

fn input_spans(
    state: &UiState,
    start: usize,
    end: usize,
    command_end_char: Option<usize>,
    command_style: Style,
) -> Vec<Span<'static>> {
    let mention_style = Style::default()
        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
        .add_modifier(Modifier::BOLD);
    let paste_marker_style = Style::default()
        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
        .add_modifier(Modifier::BOLD);
    let image_style = Style::default()
        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
        .add_modifier(Modifier::BOLD);

    let mut spans = Vec::new();
    let mut cursor = start;
    while cursor < end {
        if let Some(marker) = state.paste_marker_at(cursor) {
            let marker_end = marker.end_char.min(end);
            spans.push(Span::styled(
                chars_slice(&state.input, cursor, marker_end),
                paste_marker_style,
            ));
            cursor = marker.end_char;
        } else if let Some(image) = state.image_at(cursor) {
            let image_end = image.end_char.min(end);
            spans.push(Span::styled(
                chars_slice(&state.input, cursor, image_end),
                image_style,
            ));
            cursor = image.end_char;
        } else if let Some(mention) = state.mention_at(cursor) {
            let mention_end = mention.end_char.min(end);
            spans.push(Span::styled(
                chars_slice(&state.input, cursor, mention_end),
                mention_style,
            ));
            cursor = mention.end_char;
        } else {
            let next_special = (cursor + 1..end)
                .find(|idx| {
                    state.paste_marker_at(*idx).is_some()
                        || state.image_at(*idx).is_some()
                        || state.mention_at(*idx).is_some()
                })
                .unwrap_or(end);
            push_plain_input_segment(
                state,
                &mut spans,
                cursor,
                next_special,
                command_end_char,
                command_style,
            );
            cursor = next_special;
        }
    }
    spans
}

fn col_width(state: &UiState, line_idx: usize, col: usize) -> usize {
    let Some((start, end)) = state.input_line_bounds().get(line_idx).copied() else {
        return 0;
    };
    state.input_display_width(start, end).min(col)
}

fn push_plain_input_segment(
    state: &UiState,
    spans: &mut Vec<Span<'static>>,
    start: usize,
    end: usize,
    command_end_char: Option<usize>,
    command_style: Style,
) {
    let Some(command_end) = command_end_char else {
        spans.push(Span::raw(chars_slice(&state.input, start, end)));
        return;
    };

    if start < command_end {
        let styled_end = end.min(command_end);
        spans.push(Span::styled(
            chars_slice(&state.input, start, styled_end),
            command_style,
        ));
        if styled_end < end {
            spans.push(Span::raw(chars_slice(&state.input, styled_end, end)));
        }
    } else {
        spans.push(Span::raw(chars_slice(&state.input, start, end)));
    }
}

fn chars_slice(input: &str, start: usize, end: usize) -> String {
    input
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn matched_input_command<'a>(
    state: &'a UiState,
    input: &str,
) -> Option<&'a crate::types::events::CommandSummary> {
    input
        .starts_with('/')
        .then(|| {
            let cmd_raw = if let Some(space_pos) = input.find(' ') {
                &input[1..space_pos]
            } else {
                &input[1..]
            };
            state
                .autocomplete
                .all_commands
                .iter()
                .find(|c| c.name == cmd_raw || c.aliases.iter().any(|a| a == cmd_raw))
        })
        .flatten()
}

fn command_highlight_end(state: &UiState) -> Option<usize> {
    let input = &state.input;
    let cmd = matched_input_command(state, input)?;
    let command_end_byte = input.find(' ').unwrap_or(input.len());
    let command_end_char = input[..command_end_byte].chars().count();
    if cmd.name.is_empty() {
        None
    } else {
        Some(command_end_char)
    }
}

fn command_args_hint(state: &UiState) -> Option<&'static str> {
    let input = &state.input;
    let cmd = matched_input_command(state, input)?;
    if !cmd.has_args {
        return None;
    }

    let space_pos = input.find(' ')?;
    if input[space_pos..].chars().all(char::is_whitespace) {
        cmd.args_description
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::CommandSummary;

    fn command(
        name: &str,
        has_args: bool,
        args_description: Option<&'static str>,
    ) -> CommandSummary {
        CommandSummary {
            name: name.to_string(),
            aliases: Vec::new(),
            description: String::new(),
            sort_weight: 0,
            has_args,
            args_description,
        }
    }

    #[test]
    fn command_args_hint_shows_for_selected_arg_command_without_args() {
        let mut state = UiState::new();
        state.autocomplete.all_commands = vec![command("rename", true, Some("<name>"))];
        state.input = "/rename ".to_string();

        assert_eq!(command_args_hint(&state), Some("<name>"));
    }

    #[test]
    fn command_args_hint_hides_after_user_types_args() {
        let mut state = UiState::new();
        state.autocomplete.all_commands = vec![command("rename", true, Some("<name>"))];
        state.input = "/rename title".to_string();

        assert_eq!(command_args_hint(&state), None);
    }
}

fn render_queued_user_inputs(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    let bg_color = Color::Rgb(54, 58, 66);
    let bg = Style::default().bg(bg_color);
    frame.render_widget(Paragraph::new(Line::from("")).style(bg), area);

    if area.height < 3 {
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    let title_style = Style::default()
        .fg(Color::Rgb(235, 238, 244))
        .bg(bg_color)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let meta_style = Style::default()
        .fg(Color::Rgb(0xab, 0xab, 0xab))
        .bg(bg_color);
    let is_pending = !state.pending_intervention_inputs.is_empty();
    let input_texts = queued_drawer_inputs(state)
        .iter()
        .map(|draft| draft.text.clone())
        .collect::<Vec<_>>();
    let title = if is_pending {
        format!("插入到下一轮 ({})", input_texts.len())
    } else {
        format!("已排队消息 ({})", input_texts.len())
    };
    let title_meta = if is_pending {
        " - 等待当前轮次边界"
    } else {
        " - 当前运行结束后发送"
    };
    let mut title_line = Line::from(vec![
        Span::styled(" ", bg),
        Span::styled(title, title_style),
        Span::styled(title_meta, meta_style),
    ]);
    state.register_selectable_screen_line(
        chunks[0].y,
        chunks[0].x,
        chunks[0].width,
        line_to_plain_text(&title_line),
    );
    apply_selection_highlight(state, chunks[0], std::slice::from_mut(&mut title_line));
    frame.render_widget(Paragraph::new(title_line).style(bg), chunks[0]);

    let visible = chunks[1].height as usize;
    let skip = input_texts.len().saturating_sub(visible);
    let width = area.width as usize;
    let prefix_style = Style::default()
        .fg(Color::Rgb(0xab, 0xab, 0xab))
        .bg(bg_color);
    let text_style = Style::default().fg(Color::Rgb(205, 210, 218)).bg(bg_color);

    let mut lines: Vec<Line> = input_texts
        .iter()
        .skip(skip)
        .map(|text| {
            let prefix = "  - ";
            let available = width.saturating_sub(UnicodeWidthStr::width(prefix));
            Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled(ellipsize_width(text, available), text_style),
            ])
        })
        .collect();

    for (idx, line) in lines.iter().enumerate() {
        state.register_selectable_screen_line(
            chunks[1].y + idx as u16,
            chunks[1].x,
            chunks[1].width,
            line_to_plain_text(line),
        );
    }
    apply_selection_highlight(state, chunks[1], &mut lines);
    frame.render_widget(Paragraph::new(lines).style(bg), chunks[1]);

    let hint_style = Style::default().fg(Color::Rgb(255, 204, 163)).bg(bg_color);
    let mut footer = if is_pending {
        Line::from(vec![
            Span::styled(" ", bg),
            Span::styled("输入队列已锁定，等待插入完成", meta_style),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ", bg),
            Span::styled("Alt+Enter", hint_style),
            Span::styled(" 将排队消息插入到下一轮 LLM 前", meta_style),
        ])
    };
    state.register_selectable_screen_line(
        chunks[2].y,
        chunks[2].x,
        chunks[2].width,
        line_to_plain_text(&footer),
    );
    apply_selection_highlight(state, chunks[2], std::slice::from_mut(&mut footer));
    frame.render_widget(Paragraph::new(footer).style(bg), chunks[2]);
}

fn register_input_lines(state: &mut UiState, area: Rect, lines: &[Line<'_>]) {
    for (idx, line) in lines.iter().enumerate() {
        state.register_selectable_screen_line(
            area.y + idx as u16,
            area.x,
            area.width,
            line_to_plain_text(line),
        );
    }
}

fn apply_selection_highlight(state: &UiState, area: Rect, lines: &mut [Line<'static>]) {
    let highlight = Style::default()
        .fg(Color::Rgb(40, 44, 52))
        .bg(Color::Rgb(180, 210, 255))
        .add_modifier(Modifier::BOLD);

    for (idx, line) in lines.iter_mut().enumerate() {
        let text = line_to_plain_text(line);
        let screen_row = area.y + idx as u16;
        if let Some((start_col, end_col)) = selected_cols_for_screen_line(state, screen_row, &text)
        {
            *line = highlighted_line(&text, start_col, end_col, highlight);
        }
    }
}

fn line_to_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

pub(super) fn queued_drawer_inputs(state: &UiState) -> &std::collections::VecDeque<UserDraft> {
    if state.pending_intervention_inputs.is_empty() {
        &state.queued_user_inputs
    } else {
        &state.pending_intervention_inputs
    }
}

fn ellipsize_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let mut out = String::new();
    let mut width = 0;
    let limit = max_width - 3;
    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > limit {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push_str("...");
    out
}
