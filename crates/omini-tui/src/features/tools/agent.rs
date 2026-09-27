use crate::features::tools::{tool_error_display_text, truncate_display_width};
use crate::ui::theme;
use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub fn render(
    tool: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    width: usize,
) -> Vec<Line<'static>> {
    let name = tool
        .input
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("Agent");
    let title = tool
        .input
        .get("title")
        .or_else(|| tool.input.get("task"))
        .and_then(|value| value.as_str())
        .unwrap_or("Agent task");
    let mut heading = Line::from(vec![
        Span::styled("◆ ", Style::default().fg(theme::ACCENT)),
        Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {title}"), Style::default().fg(theme::TEXT)),
    ]);
    crate::ui::text::fit_line(&mut heading, width);
    let mut lines = vec![heading];
    if let Some(result) = result.filter(|result| result.is_error) {
        lines.push(Line::from(Span::styled(
            truncate_display_width(
                &format!("  {}", tool_error_display_text(&result.content)),
                width,
            ),
            Style::default().fg(theme::ERROR),
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn agent_launch_stays_static_after_result() {
        let input = HashMap::from([
            ("name".to_string(), serde_json::json!("Explore")),
            ("title".to_string(), serde_json::json!("梳理架构")),
        ]);
        let tool = ToolUseBlock {
            id: "agent-1".into(),
            name: "spawn_agent".into(),
            input,
        };
        let result = ToolResultBlock {
            tool_use_id: tool.id.clone(),
            is_error: false,
            content: "task-1".into(),
            metadata: None,
        };

        assert_eq!(render(&tool, None, 80), render(&tool, Some(&result), 80));
        let error = ToolResultBlock {
            is_error: true,
            content: "Cannot start agent".into(),
            ..result
        };
        let lines = render(&tool, Some(&error), 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans[0].style.fg, Some(theme::ERROR));
    }
}
