use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{spinner, tool_error_display_text, word_wrap};

pub(super) fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let dim = Color::Rgb(140, 142, 150);
    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let warn = Color::Rgb(212, 182, 106);
    let error = Color::Rgb(255, 100, 100);
    let output = Color::Rgb(156, 156, 156);

    const MAX_OUTPUT_LINES: usize = 10;

    let mut lines: Vec<Line> = Vec::new();
    let is_pending = result.is_none();
    let title_style = Style::default().fg(accent);
    let mut title = Vec::new();
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

    title.push(Span::raw("· "));
    title.push(Span::styled("Bash", title_style));
    if !cmd.is_empty() {
        let used_width: usize = title.iter().map(|s| s.width()).sum();
        let parens_width = UnicodeWidthStr::width("()");
        let spinner_width = if is_pending {
            UnicodeWidthStr::width(format!(" {}", spinner()).as_str())
        } else {
            0
        };
        let cmd_width = content_width
            .saturating_sub(used_width)
            .saturating_sub(parens_width)
            .saturating_sub(spinner_width);
        title.push(Span::raw("("));
        title.push(Span::raw(truncate_display_width(cmd, cmd_width)));
        title.push(Span::raw(")"));
    } else {
        let used_width: usize = title.iter().map(|s| s.width()).sum();
        if used_width > content_width {
            title.truncate(1);
            title.push(Span::styled(
                truncate_display_width("Bash", content_width.saturating_sub(2).max(1)),
                title_style,
            ));
        }
    }
    if is_pending {
        title.push(Span::styled(
            format!(" {}", spinner()),
            Style::default().fg(warn),
        ));
    }
    lines.push(Line::from(title));

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

    if !desc.is_empty() {
        push_indented(
            "  └─ ",
            "     ",
            format!("# {desc}"),
            Style::default().fg(dim).add_modifier(Modifier::ITALIC),
        );
    }

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
        let wrapped = word_wrap(&tr.content, content_width.saturating_sub(5).max(1));
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
            if display_idx == 0 && desc.is_empty() {
                push_indented("  └─ ", "     ", wl.clone(), out_style);
            } else {
                push_indented("     ", "     ", wl.clone(), out_style);
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

fn truncate_display_width(s: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let ellipsis = "...";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ellipsis.chars().take(max_width).collect();
    }

    let target = max_width - ellipsis_width;
    let mut result = String::new();
    let mut current_width = 0;
    for ch in s.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > target {
            break;
        }
        result.push(ch);
        current_width += ch_width;
    }
    result.push_str(ellipsis);
    result
}
