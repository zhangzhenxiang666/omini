use omini_domain::message::{ToolResultBlock, ToolUseBlock};
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
    let dim = Style::default().fg(Color::Rgb(0x6a, 0x6f, 0x78));
    let prefix = if query.is_empty() { "files" } else { query };
    // 计算可用宽度并做截断
    let used_width: usize = title.iter().map(|span| span.width()).sum();
    let separator = " ";
    let full_text = format!("{prefix} in {path}");
    let max_text_width = content_width
        .saturating_sub(used_width)
        .saturating_sub(separator.len());
    let full_width = UnicodeWidthStr::width(full_text.as_str());
    if full_width <= max_text_width {
        title.push(Span::raw(separator));
        title.push(Span::raw(format!("{prefix} ")));
        title.push(Span::styled("in", dim));
        title.push(Span::raw(format!(" {path}")));
    } else {
        let truncated = truncate_display_width(&full_text, max_text_width);
        title.push(Span::raw(separator));
        // 找到 "in " 的位置来拆分样式
        let prefix_with_space = format!("{prefix} ");
        let in_start = prefix_with_space.len();
        if truncated.len() > in_start + 2 {
            title.push(Span::raw(truncated[..in_start].to_string()));
            title.push(Span::styled("in", dim));
            title.push(Span::raw(truncated[in_start + 2..].to_string()));
        } else {
            title.push(Span::raw(truncated));
        }
    }
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
