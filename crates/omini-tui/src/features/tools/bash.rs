use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::{bash_highlight, tool_error_display_text, truncate_display_width, word_wrap};

/// Shell 标题优先呈现调用目的；无描述时直接展示单行命令。
pub fn render_preview(tool_use: &ToolUseBlock, content_width: usize) -> Vec<Line<'static>> {
    let desc = tool_use
        .input
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    let command = tool_use
        .input
        .get("command")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .replace('\n', " ↵ ");

    let mut lines = Vec::new();
    if !desc.is_empty() {
        lines.push(Line::from(Span::styled(
            desc.replace('\n', " "),
            Style::default()
                .fg(crate::ui::theme::MUTED)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    let mut command_spans = vec![Span::styled(
        "$ ",
        Style::default().fg(crate::ui::theme::ACCENT),
    )];
    command_spans.extend(bash_highlight::truncated_command_spans(
        &command,
        content_width.saturating_sub(2),
        Style::default().fg(bash_highlight::COMMAND_TEXT_FG),
    ));
    lines.push(Line::from(command_spans).style(Style::default().bg(crate::ui::theme::PANEL)));
    lines
}

pub fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let dim = crate::ui::theme::MUTED;
    let error = crate::ui::theme::ERROR;
    let output = crate::ui::theme::MUTED;

    const MAX_OUTPUT_LINES: usize = 10;

    let mut lines: Vec<Line> = Vec::new();
    let desc = tool_use
        .input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let cmd = tool_use
        .input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if !desc.is_empty() {
        lines.push(Line::from(Span::styled(
            truncate_display_width(&desc.replace('\n', " "), content_width),
            Style::default().fg(dim).add_modifier(Modifier::ITALIC),
        )));
    }
    for (index, spans) in bash_highlight::wrapped_command_spans(
        cmd,
        content_width.saturating_sub(2).max(1),
        Style::default().fg(bash_highlight::COMMAND_TEXT_FG),
    )
    .into_iter()
    .enumerate()
    {
        let mut command = vec![Span::styled(
            if index == 0 { "$ " } else { "  " },
            Style::default().fg(crate::ui::theme::ACCENT),
        )];
        command.extend(spans);
        lines.push(Line::from(command).style(Style::default().bg(crate::ui::theme::PANEL)));
    }
    let has_output = result.is_some_and(|tr| !tr.content.is_empty());

    let mut push_indented =
        |prefix: &'static str, continuation: &'static str, content: String, style: Style| {
            let prefix_width = UnicodeWidthStr::width(prefix);
            let wrap_width = content_width.saturating_sub(prefix_width).max(1);
            let wrapped = word_wrap(&content, wrap_width);
            for (idx, wl) in wrapped.into_iter().enumerate() {
                let current_prefix = if idx == 0 { prefix } else { continuation };
                lines.push(Line::from(vec![
                    Span::raw(current_prefix),
                    Span::styled(wl, style),
                ]));
            }
        };

    if let Some(tr) = result
        && tr.is_error
    {
        push_tool_error(&mut lines, &tr.content, content_width, error);
        return lines;
    }

    if let Some(tr) = result
        && has_output
    {
        let out_style = Style::default().fg(output);
        let wrapped = word_wrap(&tr.content, content_width.saturating_sub(4).max(1));
        let total = wrapped.len();
        let truncated = total > MAX_OUTPUT_LINES;

        let mut display_indices: Vec<usize> = if truncated {
            let head_count = MAX_OUTPUT_LINES / 2;
            let tail_count = MAX_OUTPUT_LINES.saturating_sub(head_count);
            let mut indices: Vec<usize> = (0..head_count).collect();
            indices.extend(total.saturating_sub(tail_count)..total);
            indices
        } else {
            (0..total).collect()
        };
        display_indices.dedup();

        for (display_idx, line_idx) in display_indices.iter().enumerate() {
            if truncated && *line_idx == total.saturating_sub(MAX_OUTPUT_LINES / 2) {
                let omitted = total.saturating_sub(MAX_OUTPUT_LINES);
                push_indented(
                    "     ",
                    "     ",
                    format!("... {omitted} lines omitted ..."),
                    Style::default().fg(dim).add_modifier(Modifier::ITALIC),
                );
            }
            let wl = &wrapped[*line_idx];
            if display_idx == 0 {
                push_indented("  └ ", "    ", wl.clone(), out_style);
            } else {
                push_indented("    ", "    ", wl.clone(), out_style);
            }
        }
    }

    lines
}

fn push_tool_error(
    lines: &mut Vec<Line<'static>>,
    content: &str,
    content_width: usize,
    error: Color,
) {
    let message = tool_error_display_text(content);
    let message = message.trim();
    let message = if message.is_empty() {
        "Tool execution failed"
    } else {
        message
    };
    let prefix = "  ";
    let continuation = "  ";
    let prefix_width = UnicodeWidthStr::width(prefix);
    let wrap_width = content_width.saturating_sub(prefix_width).max(1);
    let style = Style::default().fg(error);
    for (idx, line) in word_wrap(message, wrap_width).into_iter().enumerate() {
        let current_prefix = if idx == 0 { prefix } else { continuation };
        lines.push(Line::from(vec![
            Span::styled(current_prefix, style),
            Span::styled(line, style),
        ]));
    }
}
