use crate::features::tools::{tool_error_display_text, word_wrap};
use crate::ui::theme;
use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::{
    style::Style,
    text::{Line, Span},
};

/// 未知工具保留身份和参数；结果沿用十行摘要上限。
pub fn render(
    tool: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        tool.name.clone(),
        Style::default().fg(theme::ACCENT),
    ))];
    let input = serde_json::to_string(&tool.input).unwrap_or_default();
    section(&mut lines, "Parameters", &input, width, theme::TEXT, 6);
    if let Some(result) = result {
        let text = if result.is_error {
            tool_error_display_text(&result.content)
        } else {
            result.content.clone()
        };
        section(
            &mut lines,
            "Result",
            &text,
            width,
            if result.is_error {
                theme::ERROR
            } else {
                theme::TEXT
            },
            10,
        );
    }
    lines
}
fn section(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    text: &str,
    width: usize,
    color: ratatui::style::Color,
    limit: usize,
) {
    lines.push(Line::from(Span::styled(
        format!("  {title}"),
        Style::default().fg(theme::MUTED),
    )));
    let wrapped = word_wrap(text, width.saturating_sub(4).max(1));
    lines.extend(wrapped.iter().take(limit).map(|line| {
        Line::from(Span::styled(
            format!("    {line}"),
            Style::default().fg(color),
        ))
    }));
    if wrapped.len() > limit {
        lines.push(Line::from(Span::styled(
            format!("    … {} lines omitted", wrapped.len() - limit),
            Style::default().fg(theme::MUTED),
        )));
    }
}
