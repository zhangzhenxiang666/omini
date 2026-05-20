use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::{spinner, word_wrap};

pub(super) fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let skill_name = tool_use
        .input
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("<unknown>");
    let skill_color = Color::Rgb(0x42, 0xb3, 0xc2);
    let mut main_spans = Vec::new();

    main_spans.push(Span::raw("· "));
    main_spans.push(Span::styled("Skill", Style::default().fg(skill_color)));
    main_spans.push(Span::raw(format!(" {skill_name}")));
    if result.is_none() {
        main_spans.push(Span::styled(
            format!(" {}", spinner()),
            Style::default().fg(Color::Rgb(212, 182, 106)),
        ));
    }

    let mut lines = vec![Line::from(main_spans)];
    if let Some(tr) = result
        && tr.is_error
    {
        let error_style = Style::default().fg(Color::Rgb(255, 100, 100));
        for line in word_wrap(&tr.content, content_width.saturating_sub(2)) {
            lines.push(Line::from(Span::styled(line, error_style)));
        }
    }
    lines
}
