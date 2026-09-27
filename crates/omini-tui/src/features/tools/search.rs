use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::path::Path;

use super::{
    display_path, tool_error_display_text, tool_title_style, truncate_display_width, word_wrap,
};
use unicode_width::UnicodeWidthStr;

/// Search 标题把查询和范围放在同一行，并为路径预留空间。
pub fn render_preview(
    tool_use: &ToolUseBlock,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let query = tool_use
        .input
        .get("query")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    let query = if query.is_empty() { "files" } else { query };
    let path = tool_use
        .input
        .get("path")
        .and_then(|value| value.as_str())
        .map(|path| display_path(path, project_dir))
        .unwrap_or_else(|| ".".to_string());
    let title = "⌕ Search";
    let available = content_width.saturating_sub(UnicodeWidthStr::width(title) + 2);
    let path_budget = available.saturating_sub(5).min(available / 2);
    let path = truncate_display_width(&path.replace('\n', "↵"), path_budget);
    let location = format!(" in {path}");
    let query_budget = available.saturating_sub(UnicodeWidthStr::width(location.as_str()));
    let query = truncate_display_width(&query.replace('\n', " "), query_budget);

    vec![Line::from(vec![
        Span::styled(title, tool_title_style(crate::ui::theme::ACCENT)),
        Span::styled(
            format!("  {query}"),
            Style::default().fg(crate::ui::theme::TEXT),
        ),
        Span::styled(location, Style::default().fg(crate::ui::theme::MUTED)),
    ])]
}

pub fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let error = crate::ui::theme::ERROR;

    let mut lines = render_preview(tool_use, content_width, project_dir);
    if let Some(metadata) = result.and_then(|result| result.metadata.as_ref()) {
        let stats = ["shown", "total", "files_with_matches"]
            .into_iter()
            .filter_map(|key| metadata.get(key).map(|value| format!("{key}: {value}")))
            .collect::<Vec<_>>()
            .join(" · ");
        if !stats.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {stats}"),
                Style::default().fg(crate::ui::theme::ACCENT),
            )));
        }
        if metadata.get("truncated").and_then(|value| value.as_bool()) == Some(true) {
            lines.push(Line::from(Span::styled(
                "  结果已截断",
                Style::default().fg(crate::ui::theme::MUTED),
            )));
        }
    }

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
