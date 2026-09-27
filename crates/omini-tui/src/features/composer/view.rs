use crate::app::state::AppState;
use crate::ui::context::ViewContext;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub fn render_input(state: &mut ViewContext<'_>, frame: &mut ratatui::Frame, area: Rect) {
    let bg = Style::default()
        .fg(crate::ui::theme::TEXT)
        .bg(crate::ui::theme::INPUT_BG);

    let bg_widget = Paragraph::new(Line::from("")).style(bg);
    frame.render_widget(bg_widget, area);
    let visible_line_count = state.composer.input_visible_line_count();

    let drawer_len = state.composer.queued_user_inputs.len();
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
    for border in [chunks[0], chunks[2]] {
        frame.render_widget(
            Paragraph::new("─".repeat(border.width as usize))
                .style(bg.fg(crate::ui::theme::BORDER)),
            border,
        );
    }

    let line_bg = Paragraph::new(Line::from(Span::styled(
        " ".repeat(area.width as usize),
        bg,
    )))
    .style(bg);
    frame.render_widget(line_bg, input_body);

    let prefix_style = Style::default().fg(crate::ui::theme::MUTED);
    let cmd_color = Style::default().fg(crate::ui::theme::ACCENT);
    let read_only_task = state.session_is_terminal();
    let lines = if state.composer.input.is_empty() {
        let mut spans = vec![Span::styled("\u{276f} ", prefix_style)];
        if read_only_task {
            spans.push(Span::styled(
                "此任务已结束，只能查看历史",
                Style::default()
                    .fg(crate::ui::theme::MUTED)
                    .bg(crate::ui::theme::PANEL),
            ));
        } else {
            // 在原生光标位置留一格，避免空输入时遮住建议的首字。
            spans.push(Span::styled(
                format!(" {}", state.composer.input_placeholder),
                Style::default()
                    .fg(crate::ui::theme::MUTED)
                    .bg(crate::ui::theme::INPUT_BG)
                    .add_modifier(Modifier::DIM),
            ));
        }
        vec![Line::from(spans)]
    } else {
        input_lines(state, prefix_style, cmd_color)
    };
    if !state.composer.input.is_empty() {
        register_input_lines(state, input_body, &lines);
    }
    let paragraph = Paragraph::new(lines).style(bg);
    frame.render_widget(paragraph, input_body);

    let (cursor_x, cursor_y) = if state.composer.input.is_empty() {
        (input_body.x + 2, input_body.y)
    } else {
        let (line_idx, col) = state.composer.input_cursor_line_col().unwrap_or((0, 0));
        let visible_line = line_idx.saturating_sub(state.composer.input_scroll_line);
        let x_offset = state.composer.input_visual_line_prefix_width(line_idx) as u16;
        (
            input_body.x + x_offset + col_width(state, line_idx, col) as u16,
            input_body.y + visible_line as u16,
        )
    };
    if !read_only_task && input_body.width > 2 && input_body.height > 0 {
        frame.set_cursor_position((
            cursor_x.min(input_body.right().saturating_sub(1)),
            cursor_y.min(input_body.bottom().saturating_sub(1)),
        ));
    }
}

fn input_lines(
    state: &ViewContext<'_>,
    prefix_style: Style,
    cmd_color: Style,
) -> Vec<Line<'static>> {
    let command_end_char = command_highlight_end(state);
    let args_hint = command_args_hint(state);
    let input_char_len = state.composer.input.chars().count();
    let hint_style = Style::default().fg(crate::ui::theme::MUTED);
    state
        .composer
        .input_line_bounds()
        .into_iter()
        .enumerate()
        .skip(state.composer.input_scroll_line)
        .take(state.composer.input_visible_line_count())
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
    state: &ViewContext<'_>,
    start: usize,
    end: usize,
    command_end_char: Option<usize>,
    command_style: Style,
) -> Vec<Span<'static>> {
    let mention_style = Style::default()
        .fg(crate::ui::theme::ACCENT)
        .add_modifier(Modifier::BOLD);
    let paste_marker_style = Style::default()
        .fg(crate::ui::theme::ACCENT)
        .add_modifier(Modifier::BOLD);
    let image_style = Style::default()
        .fg(crate::ui::theme::ACCENT)
        .add_modifier(Modifier::BOLD);

    let mut spans = Vec::new();
    let mut cursor = start;
    while cursor < end {
        if let Some(marker) = state.composer.paste_marker_at(cursor) {
            let marker_end = marker.end_char.min(end);
            spans.push(Span::styled(
                chars_slice(&state.composer.input, cursor, marker_end),
                paste_marker_style,
            ));
            cursor = marker.end_char;
        } else if let Some(image) = state.composer.image_at(cursor) {
            let image_end = image.end_char.min(end);
            spans.push(Span::styled(
                chars_slice(&state.composer.input, cursor, image_end),
                image_style,
            ));
            cursor = image.end_char;
        } else if let Some(mention) = state.composer.mention_at(cursor) {
            let mention_end = mention.end_char.min(end);
            spans.push(Span::styled(
                chars_slice(&state.composer.input, cursor, mention_end),
                mention_style,
            ));
            cursor = mention.end_char;
        } else {
            let next_special = (cursor + 1..end)
                .find(|idx| {
                    state.composer.paste_marker_at(*idx).is_some()
                        || state.composer.image_at(*idx).is_some()
                        || state.composer.mention_at(*idx).is_some()
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

fn col_width(state: &ViewContext<'_>, line_idx: usize, col: usize) -> usize {
    let Some((start, end)) = state.composer.input_line_bounds().get(line_idx).copied() else {
        return 0;
    };
    state.composer.input_display_width(start, end).min(col)
}

fn push_plain_input_segment(
    state: &ViewContext<'_>,
    spans: &mut Vec<Span<'static>>,
    start: usize,
    end: usize,
    command_end_char: Option<usize>,
    command_style: Style,
) {
    let Some(command_end) = command_end_char else {
        spans.push(Span::raw(chars_slice(&state.composer.input, start, end)));
        return;
    };

    if start < command_end {
        let styled_end = end.min(command_end);
        spans.push(Span::styled(
            chars_slice(&state.composer.input, start, styled_end),
            command_style,
        ));
        if styled_end < end {
            spans.push(Span::raw(chars_slice(
                &state.composer.input,
                styled_end,
                end,
            )));
        }
    } else {
        spans.push(Span::raw(chars_slice(&state.composer.input, start, end)));
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
    state: &'a AppState,
    input: &str,
) -> Option<&'a crate::app::event::CommandSummary> {
    input
        .starts_with('/')
        .then(|| {
            let cmd_raw = if let Some(space_pos) = input.find(' ') {
                &input[1..space_pos]
            } else {
                &input[1..]
            };
            state
                .composer
                .autocomplete
                .all_commands
                .iter()
                .find(|c| c.name == cmd_raw || c.aliases.iter().any(|a| a == cmd_raw))
        })
        .flatten()
}

fn command_highlight_end(state: &ViewContext<'_>) -> Option<usize> {
    let input = &state.composer.input;
    let cmd = matched_input_command(state, input)?;
    let command_end_byte = input.find(' ').unwrap_or(input.len());
    let command_end_char = input[..command_end_byte].chars().count();
    if cmd.name.is_empty() {
        None
    } else {
        Some(command_end_char)
    }
}

fn command_args_hint<'a>(state: &'a ViewContext<'_>) -> Option<&'a str> {
    let input = &state.composer.input;
    let cmd = matched_input_command(state, input)?;
    if !cmd.has_args {
        return None;
    }

    let space_pos = input.find(' ')?;
    if input[space_pos..].chars().all(char::is_whitespace) {
        cmd.args_description.as_deref()
    } else {
        None
    }
}

fn render_queued_user_inputs(state: &mut ViewContext<'_>, frame: &mut ratatui::Frame, area: Rect) {
    let bg_color = crate::ui::theme::PANEL;
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
        .fg(crate::ui::theme::TEXT)
        .bg(bg_color)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let meta_style = Style::default().fg(crate::ui::theme::MUTED).bg(bg_color);
    let input_texts = state
        .composer
        .queued_user_inputs
        .iter()
        .map(|draft| draft.text.clone())
        .collect::<Vec<_>>();
    let title = format!("已排队消息 ({})", input_texts.len());
    let title_meta = " - 未插入时在当前运行结束后发送";
    let title_line = Line::from(vec![
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
    frame.render_widget(Paragraph::new(title_line).style(bg), chunks[0]);

    let visible = chunks[1].height as usize;
    let skip = input_texts.len().saturating_sub(visible);
    let width = area.width as usize;
    let prefix_style = Style::default().fg(crate::ui::theme::MUTED).bg(bg_color);
    let text_style = Style::default().fg(crate::ui::theme::TEXT).bg(bg_color);

    let lines: Vec<Line> = input_texts
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
    frame.render_widget(Paragraph::new(lines).style(bg), chunks[1]);

    let hint_style = Style::default().fg(crate::ui::theme::SELECTED).bg(bg_color);
    let footer = Line::from(vec![
        Span::styled(" ", bg),
        Span::styled("Alt+Enter", hint_style),
        Span::styled(" 立即提交排队输入", meta_style),
    ]);
    state.register_selectable_screen_line(
        chunks[2].y,
        chunks[2].x,
        chunks[2].width,
        line_to_plain_text(&footer),
    );
    frame.render_widget(Paragraph::new(footer).style(bg), chunks[2]);
}

fn register_input_lines(state: &mut ViewContext<'_>, area: Rect, lines: &[Line<'_>]) {
    for (idx, line) in lines.iter().enumerate() {
        state.register_selectable_screen_line(
            area.y + idx as u16,
            area.x,
            area.width,
            line_to_plain_text(line),
        );
    }
}

fn line_to_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
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

#[cfg(test)]
mod tests {

    use super::*;
    use crate::app::event::{CommandKind, CommandSummary};

    fn command(name: &str, has_args: bool, args_description: Option<&str>) -> CommandSummary {
        CommandSummary {
            name: name.to_string(),
            aliases: Vec::new(),
            description: String::new(),
            sort_weight: 0,
            kind: CommandKind::Builtin,
            has_args,
            args_description: args_description.map(str::to_string),
        }
    }

    #[test]
    fn command_args_hint_shows_for_selected_arg_command_without_args() {
        let mut state = AppState::new();
        state.composer.autocomplete.all_commands = vec![command("rename", true, Some("<name>"))];
        state.composer.input = "/rename ".to_string();

        assert_eq!(command_args_hint(&ViewContext::new(&state)), Some("<name>"));
    }

    #[test]
    fn command_args_hint_hides_after_user_types_args() {
        let mut state = AppState::new();
        state.composer.autocomplete.all_commands = vec![command("rename", true, Some("<name>"))];
        state.composer.input = "/rename title".to_string();

        assert_eq!(command_args_hint(&ViewContext::new(&state)), None);
    }
}
