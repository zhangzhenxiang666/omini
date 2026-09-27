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

/// 渲染 `send_message` 的分界条目：`↪ 收件方 · 消息首行`。
/// `target_label` 是按 task ID 解析出的子任务标题；未解析时回退到原始 target 文本，
/// 因此无会话状态的调用点（权限抽屉、调试快照）也能得到可读条目。
pub fn render_send_message(
    tool: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    target_label: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let target = tool
        .input
        .get("target")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    // 只展示首行：完整正文出现在收件方会话里，主时间线只需要发送方向和摘要。
    let first_line = tool
        .input
        .get("message")
        .and_then(|value| value.as_str())
        .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or_default();
    let label = target_label
        .filter(|label| !label.is_empty())
        .unwrap_or(target);
    let mut spans = vec![Span::styled("↪ ", Style::default().fg(theme::ACCENT))];
    if !label.is_empty() {
        spans.push(Span::styled(
            label.to_string(),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !first_line.is_empty() {
        let body = if label.is_empty() {
            first_line.to_string()
        } else {
            format!(" · {first_line}")
        };
        spans.push(Span::styled(body, Style::default().fg(theme::TEXT)));
    }
    let mut heading = Line::from(spans);
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

    fn send_message_tool(target: &str, message: &str) -> ToolUseBlock {
        ToolUseBlock {
            id: "send-1".into(),
            name: "send_message".into(),
            input: HashMap::from([
                ("target".to_string(), serde_json::json!(target)),
                ("message".to_string(), serde_json::json!(message)),
            ]),
        }
    }

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn send_message_shows_resolved_label_and_first_line() {
        let tool = send_message_tool("task-1", "顺带检查边界情况\n其余细节稍后说明");

        let lines = render_send_message(&tool, None, Some("梳理架构"), 80);

        assert_eq!(plain(&lines[0]), "↪ 梳理架构 · 顺带检查边界情况");
    }

    #[test]
    fn send_message_falls_back_to_raw_target_without_label() {
        let tool = send_message_tool("task-9", "继续之前的搜索");

        let lines = render_send_message(&tool, None, None, 80);

        assert_eq!(plain(&lines[0]), "↪ task-9 · 继续之前的搜索");
    }

    #[test]
    fn send_message_parent_target_uses_raw_text() {
        // 子 Agent 发往父会话时 target 为 "parent"，不会命中子任务节点。
        let tool = send_message_tool("parent", "汇报当前进度");

        let lines = render_send_message(&tool, None, None, 80);

        assert_eq!(plain(&lines[0]), "↪ parent · 汇报当前进度");
    }

    #[test]
    fn send_message_blank_message_keeps_label_without_separator() {
        let tool = send_message_tool("task-1", "   ");

        let lines = render_send_message(&tool, None, Some("Explore"), 80);

        assert_eq!(plain(&lines[0]), "↪ Explore");
    }

    #[test]
    fn send_message_truncates_to_width_and_reports_error() {
        let tool = send_message_tool("task-1", "一条相当长的消息，超出可用宽度后应被截断");
        let lines = render_send_message(&tool, None, Some("Explore"), 16);
        let text = plain(&lines[0]);
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= 16,
            "{text}"
        );

        let error = ToolResultBlock {
            tool_use_id: tool.id.clone(),
            is_error: true,
            content: "unknown agent task 'task-1'".into(),
            metadata: None,
        };
        let lines = render_send_message(&tool, Some(&error), Some("Explore"), 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans[0].style.fg, Some(theme::ERROR));
    }
}
