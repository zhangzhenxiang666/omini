use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{display_path, tool_error_display_text, tool_title_style, word_wrap};

pub(super) fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let error = Color::Rgb(255, 100, 100);

    let mut lines: Vec<Line> = Vec::new();
    let is_pending = result.is_none();
    let title_style = tool_title_style(accent, is_pending);
    let mut title = Vec::new();

    let query = tool_use
        .input
        .get("query")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    let path = tool_use
        .input
        .get("path")
        .and_then(|value| value.as_str())
        .map(|path| display_path(path, project_dir))
        .unwrap_or_else(|| ".".to_string());

    title.push(Span::raw("· "));
    title.push(Span::styled("Search", title_style));
    let detail = search_summary(query, &path);
    let used_width: usize = title.iter().map(|span| span.width()).sum();
    let detail_width = content_width.saturating_sub(used_width).saturating_sub(1);
    title.push(Span::raw(" "));
    title.push(Span::raw(truncate_display_width(&detail, detail_width)));
    lines.push(Line::from(title));

    if let Some(tr) = result
        && tr.is_error
    {
        let error_style = Style::default().fg(error);
        let display = tool_error_display_text(&tr.content);
        for line in word_wrap(&display, content_width.saturating_sub(2).max(1)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line, error_style),
            ]));
        }
    }

    lines
}

fn search_summary(query: &str, path: &str) -> String {
    if query.is_empty() {
        format!("files in {path}")
    } else {
        format!("{query} in {path}")
    }
}

fn truncate_display_width(s: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }

    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width + 1 > max_width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}
