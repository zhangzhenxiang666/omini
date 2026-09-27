use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::{tool_error_display_text, tool_title_style, word_wrap};

struct TodoItem {
    content: String,
    status: TodoStatus,
}

#[derive(Clone, Copy)]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
    Unknown,
}

pub fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let accent = crate::ui::theme::ACCENT;
    let error = crate::ui::theme::ERROR;
    let dim = crate::ui::theme::MUTED;

    let mut lines = Vec::new();
    let title = vec![
        Span::raw("⏺ "),
        Span::styled("Checklist", tool_title_style(accent)),
    ];
    lines.push(Line::from(title));

    if let Some(tr) = result
        && tr.is_error
    {
        push_error(&mut lines, &tr.content, content_width, error);
        return lines;
    }

    let todos = todo_items(tool_use);
    if todos.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  └ "),
            Span::styled("No todo items", Style::default().fg(dim)),
        ]));
        return lines;
    }

    for (idx, todo) in todos.iter().enumerate() {
        push_todo_item(&mut lines, todo, idx == 0, content_width);
    }

    lines
}

fn todo_items(tool_use: &ToolUseBlock) -> Vec<TodoItem> {
    tool_use
        .input
        .get("todos")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let content = item.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .map(TodoStatus::from_str)
                .unwrap_or(TodoStatus::Unknown);
            Some(TodoItem {
                content: content.to_string(),
                status,
            })
        })
        .collect()
}

impl TodoStatus {
    fn from_str(value: &str) -> Self {
        match value {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Unknown,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Pending => "□",
            Self::InProgress => "◐",
            Self::Completed => "✔",
            Self::Cancelled => "—",
            Self::Unknown => "?",
        }
    }

    fn symbol_style(self) -> Style {
        Style::default().fg(match self {
            Self::InProgress => crate::ui::theme::RUNNING,
            Self::Completed => crate::ui::theme::SUCCESS,
            _ => crate::ui::theme::MUTED,
        })
    }

    fn content_style(self) -> Style {
        match self {
            Self::Pending => Style::default().fg(crate::ui::theme::MUTED),
            Self::InProgress => Style::default()
                .fg(crate::ui::theme::ACCENT)
                .add_modifier(Modifier::BOLD),
            Self::Completed => Style::default()
                .fg(crate::ui::theme::MUTED)
                .add_modifier(Modifier::CROSSED_OUT),
            Self::Cancelled => Style::default()
                .fg(crate::ui::theme::BORDER)
                .add_modifier(Modifier::CROSSED_OUT),
            Self::Unknown => Style::default().fg(crate::ui::theme::MUTED),
        }
    }
}

fn push_todo_item(
    lines: &mut Vec<Line<'static>>,
    todo: &TodoItem,
    first: bool,
    content_width: usize,
) {
    let prefix = if first { "  └ " } else { "    " };
    let continuation = "       ";
    let symbol = todo.status.symbol();
    let symbol_style = todo.status.symbol_style();
    let content_style = todo.status.content_style();
    let used_width = UnicodeWidthStr::width(prefix)
        + UnicodeWidthStr::width(symbol)
        + UnicodeWidthStr::width(" ");
    let wrap_width = content_width.saturating_sub(used_width).max(1);

    for (idx, line) in word_wrap(&todo.content, wrap_width).into_iter().enumerate() {
        if idx == 0 {
            lines.push(Line::from(vec![
                Span::raw(prefix),
                Span::styled(symbol, symbol_style),
                Span::raw(" "),
                Span::styled(line, content_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw(continuation),
                Span::styled(line, content_style),
            ]));
        }
    }
}

fn push_error(lines: &mut Vec<Line<'static>>, content: &str, content_width: usize, error: Color) {
    let message = tool_error_display_text(content);
    let message = message.trim();
    let message = if message.is_empty() {
        "Tool execution failed"
    } else {
        message
    };
    let prefix = "  └ ";
    let continuation = "    ";
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
