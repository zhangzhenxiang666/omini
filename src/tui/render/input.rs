use crate::tui::state::UiState;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub(super) fn render_input(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let bg = Style::default().bg(Color::Rgb(65, 69, 76));

    let bg_widget = Paragraph::new(Line::from("")).style(bg);
    frame.render_widget(bg_widget, area);

    let drawer_len = queued_drawer_inputs(state).len();
    let queued_height = if drawer_len == 0 {
        0
    } else {
        drawer_len.min(4) as u16 + 2
    };
    let input_area = if queued_height > 0 {
        let chunks = Layout::vertical([Constraint::Length(queued_height), Constraint::Length(3)])
            .split(area);
        render_queued_user_inputs(state, frame, chunks[0]);
        chunks[1]
    } else {
        area
    };

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(input_area);
    let input_line = chunks[1];

    let line_bg = Paragraph::new(Line::from(Span::styled(
        " ".repeat(area.width as usize),
        bg,
    )))
    .style(bg);
    frame.render_widget(line_bg, input_line);

    let prefix_style = Style::default().fg(Color::Rgb(0xab, 0xab, 0xab));
    let cmd_color = Style::default().fg(Color::Rgb(0x42, 0xd9, 0xe8));
    let placeholder_style = Style::default().fg(Color::DarkGray);

    let command_match = matched_input_command(state, &state.input);

    let content = if state.input.is_empty() {
        Line::from(vec![
            Span::styled("\u{276f} ", prefix_style),
            Span::styled("Type a message...", placeholder_style),
        ])
    } else {
        let input = &state.input;
        if let Some(cmd) = command_match {
            let after_cmd = if let Some(space_pos) = input.find(' ') {
                &input[space_pos..]
            } else {
                ""
            };
            let mut spans = vec![
                Span::styled("\u{276f} ", prefix_style),
                Span::styled(format!("/{}", cmd.name), cmd_color),
            ];
            if !after_cmd.is_empty() {
                spans.push(Span::raw(after_cmd));
                if cmd.has_args
                    && after_cmd == " "
                    && let Some(ref desc) = cmd.args_description
                {
                    spans.push(Span::styled(desc.to_string(), placeholder_style));
                }
            }
            Line::from(spans)
        } else {
            Line::from(vec![
                Span::styled("\u{276f} ", prefix_style),
                Span::raw(input),
            ])
        }
    };
    let paragraph = Paragraph::new(content);
    frame.render_widget(paragraph, input_line);

    let cursor_x = if state.input.is_empty() {
        input_line.x + 2
    } else if let Some(cmd) = command_match {
        input_line.x + 2 + rendered_command_input_width(state, cmd) as u16
    } else {
        let byte_idx = state.char_to_byte(state.cursor_char);
        let prefix_width = UnicodeWidthStr::width(&state.input[..byte_idx]);
        input_line.x + 2 + prefix_width as u16
    };
    frame.set_cursor_position((cursor_x, input_line.y));
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

fn rendered_command_input_width(
    state: &UiState,
    cmd: &crate::types::events::CommandSummary,
) -> usize {
    let input = &state.input;
    let command_end_byte = input.find(' ').unwrap_or(input.len());
    let command_end_char = input[..command_end_byte].chars().count();

    if state.cursor_char <= command_end_char {
        let rendered_command = format!("/{}", cmd.name);
        if state.cursor_char == command_end_char {
            return UnicodeWidthStr::width(rendered_command.as_str());
        }
        let partial = rendered_command
            .chars()
            .take(state.cursor_char)
            .collect::<String>();
        return UnicodeWidthStr::width(partial.as_str());
    }

    let rendered_command_width = UnicodeWidthStr::width(format!("/{}", cmd.name).as_str());
    let suffix_start = state.char_to_byte(command_end_char);
    let cursor_byte = state.char_to_byte(state.cursor_char);
    rendered_command_width + UnicodeWidthStr::width(&input[suffix_start..cursor_byte])
}

fn render_queued_user_inputs(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
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
    let inputs = queued_drawer_inputs(state);
    let title = if is_pending {
        format!("Inserting next turn ({})", inputs.len())
    } else {
        format!("Queued messages ({})", inputs.len())
    };
    let title_meta = if is_pending {
        " - waiting for the current turn boundary"
    } else {
        " - sent after the current run"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", bg),
            Span::styled(title, title_style),
            Span::styled(title_meta, meta_style),
        ]))
        .style(bg),
        chunks[0],
    );

    let visible = chunks[1].height as usize;
    let skip = inputs.len().saturating_sub(visible);
    let width = area.width as usize;
    let prefix_style = Style::default()
        .fg(Color::Rgb(0xab, 0xab, 0xab))
        .bg(bg_color);
    let text_style = Style::default().fg(Color::Rgb(205, 210, 218)).bg(bg_color);

    let lines: Vec<Line> = inputs
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

    frame.render_widget(Paragraph::new(lines).style(bg), chunks[1]);

    let hint_style = Style::default().fg(Color::Rgb(255, 204, 163)).bg(bg_color);
    let footer = if is_pending {
        Line::from(vec![
            Span::styled(" ", bg),
            Span::styled(
                "input queue is locked until insertion completes",
                meta_style,
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ", bg),
            Span::styled("Alt+Enter", hint_style),
            Span::styled(
                " inserts queued messages before the next LLM turn",
                meta_style,
            ),
        ])
    };
    frame.render_widget(Paragraph::new(footer).style(bg), chunks[2]);
}

pub(super) fn queued_drawer_inputs(state: &UiState) -> &std::collections::VecDeque<String> {
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
