use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::path::Path;

use super::{display_path, tool_error_display_text, tool_title_style, word_wrap};

pub fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let accent = crate::ui::theme::ACCENT;
    let error = crate::ui::theme::ERROR;

    let mut lines: Vec<Line> = Vec::new();
    let title_style = tool_title_style(accent);

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

    lines.push(Line::from(vec![
        Span::styled("⌕ Search", title_style),
        Span::styled(
            format!("  {}", if query.is_empty() { "files" } else { query }),
            Style::default().fg(crate::ui::theme::TEXT),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!("  in {path}"),
        Style::default().fg(crate::ui::theme::MUTED),
    )));
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
