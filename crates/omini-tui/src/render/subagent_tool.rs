use super::{status, truncate_str};
use crate::state::SubagentNode;
use crate::types::events::SubagentStatus;
use crate::types::message::{ContentBlock, ToolResultBlock, ToolUseBlock};
use crate::widgets::display_path;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn render_subagent_tool(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    node: Option<&SubagentNode>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(0x42, 0xd9, 0xe8);
    let dim = Color::Rgb(140, 145, 155);
    let text = Color::Rgb(220, 220, 225);
    let label = node
        .map(|node| node.agent_label.as_str())
        .or_else(|| tool_use.input.get("name").and_then(|value| value.as_str()))
        .unwrap_or("Subagent");
    let label = format_subagent_label(label);
    let status = node
        .map(|node| node.status)
        .or_else(|| {
            result.map(|result| {
                if result.is_error {
                    SubagentStatus::Failed
                } else {
                    SubagentStatus::Completed
                }
            })
        })
        .unwrap_or(SubagentStatus::Running);

    let mut header = vec![Span::raw("· ")];
    if matches!(status, SubagentStatus::Running) {
        header.extend(status::animated_status_spans_with_palette(
            &label,
            accent,
            Color::Rgb(0x1f, 0x4e, 0x58),
        ));
    } else {
        header.push(Span::styled(
            label,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(title) = tool_use
        .input
        .get("title")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        let used_width: usize = header
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let title_width = content_width.saturating_sub(used_width + 3);
        if title_width >= 8 {
            header.push(Span::styled(" · ", Style::default().fg(dim)));
            header.push(Span::styled(
                truncate_to_width(title, title_width),
                Style::default().fg(dim),
            ));
        }
    }

    let mut lines = vec![Line::from(header)];

    let Some(node) = node else {
        push_subagent_error_lines(&mut lines, result, content_width, "  ");
        return lines;
    };

    let mut seen_tools = HashSet::new();
    let mut child_tools = Vec::new();
    for message in &node.messages {
        for block in &message.content {
            let ContentBlock::ToolUse(child_tool) = block else {
                continue;
            };
            if !seen_tools.insert(child_tool.id.clone()) {
                continue;
            }
            child_tools.push(child_tool);
        }
    }

    let total_tools = child_tools.len();
    let mut rendered_tools = 0usize;
    for (idx, child_tool) in child_tools.iter().enumerate() {
        if total_tools > 6 && idx == 3 {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled("...", Style::default().fg(dim)),
            ]));
        }
        if total_tools > 6 && idx >= 3 && idx < total_tools.saturating_sub(3) {
            continue;
        }

        let prefix = if rendered_tools == 0 {
            "  └─ "
        } else {
            "     "
        };
        let tool_name = format_tool_label(&child_tool.name);
        let mut spans = vec![
            Span::raw(prefix),
            Span::styled(tool_name, Style::default().fg(text)),
        ];
        if let Some(summary) = subagent_tool_summary(child_tool, project_dir) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                truncate_str(&summary, content_width.saturating_sub(10)),
                Style::default().fg(dim),
            ));
        }
        lines.push(Line::from(spans));
        rendered_tools += 1;
    }

    push_subagent_error_lines(&mut lines, result, content_width, "  ");
    lines
}

fn push_subagent_error_lines(
    lines: &mut Vec<Line<'static>>,
    result: Option<&ToolResultBlock>,
    content_width: usize,
    indent: &'static str,
) {
    let Some(result) = result.filter(|result| result.is_error) else {
        return;
    };

    let error_style = Style::default().fg(Color::Rgb(255, 100, 100));
    let content = if result.content.trim().is_empty() {
        "Subagent failed"
    } else {
        result.content.trim()
    };
    let indent_width = UnicodeWidthStr::width(indent);
    let wrapped =
        crate::widgets::word_wrap(content, content_width.saturating_sub(indent_width).max(1));
    for line in wrapped {
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(line, error_style),
        ]));
    }
}

fn format_subagent_label(label: &str) -> String {
    let words = label_words(label);
    if words.is_empty() {
        return "Subagent".to_string();
    }

    let mut out = String::new();
    for word in words {
        push_capitalized(&mut out, &word);
    }
    out
}

fn format_tool_label(name: &str) -> String {
    match name {
        "ask_user" => "AskUser".to_string(),
        "todo_write" => "UpdateTodo".to_string(),
        other => {
            let words = label_words(other);
            if words.is_empty() {
                return other.to_string();
            }

            let mut out = String::new();
            for word in words {
                push_capitalized(&mut out, &word);
            }
            out
        }
    }
}

fn label_words(label: &str) -> Vec<String> {
    label_camel_boundaries(label)
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect()
}

fn label_camel_boundaries(label: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;

    for ch in label.chars() {
        if let Some(prev) = prev
            && prev.is_ascii_lowercase()
            && ch.is_ascii_uppercase()
        {
            out.push('-');
        }
        out.push(ch);
        prev = Some(ch);
    }

    out
}

fn push_capitalized(out: &mut String, word: &str) {
    let mut chars = word.chars();
    if let Some(first) = chars.next() {
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
    }
}

fn subagent_tool_summary(tool_use: &ToolUseBlock, project_dir: Option<&Path>) -> Option<String> {
    match tool_use.name.as_str() {
        "bash" => tool_use
            .input
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        "search" => {
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
            Some(if query.is_empty() {
                format!("files in {path}")
            } else {
                format!("{query} in {path}")
            })
        }
        "read" => tool_use
            .input
            .get("file_path")
            .and_then(|value| value.as_str())
            .map(|path| display_path(path, project_dir)),
        "skill" => tool_use
            .input
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        "todo_write" => tool_use
            .input
            .get("todos")
            .and_then(|value| value.as_array())
            .map(|todos| format!("{} todo item(s)", todos.len())),
        "edit" | "write" => tool_use
            .input
            .get("file_path")
            .and_then(|value| value.as_str())
            .map(|path| display_path(path, project_dir)),
        "ask_user" => Some("waiting for user input".to_string()),
        _ => None,
    }
}

fn truncate_to_width(value: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let width = UnicodeWidthStr::width(value);
    if width <= max_width {
        return value.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width + 1 >= max_width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::line_to_plain_text;

    #[test]
    fn rejected_subagent_without_started_event_renders_finished() {
        let tool_use = ToolUseBlock {
            id: "tool_1".to_string(),
            name: "subagent".to_string(),
            input: std::collections::HashMap::from([(
                "name".to_string(),
                serde_json::Value::String("explorer".to_string()),
            )]),
        };
        let result = ToolResultBlock {
            tool_use_id: "tool_1".to_string(),
            is_error: true,
            content: "subagent is not available in plan profile".to_string(),
            metadata: None,
        };

        let lines = render_subagent_tool(&tool_use, Some(&result), None, 80, None);
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert_eq!(lines[0].spans.len(), 2);
        assert!(rendered[0].contains("Explorer"));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("subagent is not available"))
        );
    }
}
