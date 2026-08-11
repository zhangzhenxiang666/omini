use super::truncate_str;
use crate::state::{SubagentNode, pause_preview_tool_use_id};
use crate::types::events::{AgentTaskStatus, ToolPauseKind, ToolPauseRequest};
use crate::widgets::{display_path, tool_title_style};
use omini_domain::message::{ContentBlock, ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn render_subagent_tool(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    node: Option<&SubagentNode>,
    pending_tool_pauses: &VecDeque<ToolPauseRequest>,
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
                    AgentTaskStatus::Failed
                } else {
                    AgentTaskStatus::Completed
                }
            })
        })
        .unwrap_or(AgentTaskStatus::Running);

    let mut header = vec![Span::raw("· ")];
    if matches!(
        status,
        AgentTaskStatus::Running | AgentTaskStatus::Cancelling
    ) {
        header.push(Span::styled(label, tool_title_style(accent, true)));
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

    let visible_tool_indices = visible_child_tool_indices(&child_tools, node, pending_tool_pauses);
    let mut previous_idx = None;
    for (rendered_tools, idx) in visible_tool_indices.into_iter().enumerate() {
        if previous_idx.is_some_and(|previous| idx > previous + 1) {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled("...", Style::default().fg(dim)),
            ]));
        }

        let child_tool = child_tools[idx];
        let tool_pause = tool_pause_for_child_tool(pending_tool_pauses, node, &child_tool.id);
        let tool_pause_active = tool_pause.is_some_and(|pause| {
            pending_tool_pauses
                .front()
                .is_some_and(|active| active.tool_use_id == pause.tool_use_id)
        });
        let prefix = if tool_pause_active {
            "  • "
        } else if rendered_tools == 0 {
            "  └ "
        } else {
            "    "
        };
        let tool_name = format_tool_label(&child_tool.name);
        let mut spans = vec![
            Span::raw(prefix),
            Span::styled(tool_name, Style::default().fg(text)),
        ];
        let pause_label = tool_pause.map(tool_pause_label);
        if let Some(summary) = subagent_tool_summary(child_tool, project_dir) {
            let pause_width = pause_label
                .map(|label| UnicodeWidthStr::width(label) + UnicodeWidthStr::width(" · "))
                .unwrap_or(0);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                truncate_str(&summary, content_width.saturating_sub(10 + pause_width)),
                Style::default().fg(dim),
            ));
        }
        if let Some(label) = pause_label {
            let status_style = if tool_pause_active {
                Style::default().fg(accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(dim)
            };
            spans.push(Span::styled(" · ", Style::default().fg(dim)));
            spans.push(Span::styled(label, status_style));
        }
        lines.push(Line::from(spans));
        previous_idx = Some(idx);
    }

    push_subagent_error_lines(&mut lines, result, content_width, "  ");
    lines
}

fn visible_child_tool_indices(
    child_tools: &[&ToolUseBlock],
    node: &SubagentNode,
    pending_tool_pauses: &VecDeque<ToolPauseRequest>,
) -> Vec<usize> {
    if child_tools.len() <= 6 {
        return (0..child_tools.len()).collect();
    }

    let mut indices = BTreeSet::new();
    for idx in 0..3.min(child_tools.len()) {
        indices.insert(idx);
    }
    for idx in child_tools.len().saturating_sub(3)..child_tools.len() {
        indices.insert(idx);
    }
    for (idx, child_tool) in child_tools.iter().enumerate() {
        if tool_pause_for_child_tool(pending_tool_pauses, node, &child_tool.id).is_some() {
            indices.insert(idx);
        }
    }

    indices.into_iter().collect()
}

fn tool_pause_for_child_tool<'a>(
    pending_tool_pauses: &'a VecDeque<ToolPauseRequest>,
    node: &SubagentNode,
    tool_use_id: &str,
) -> Option<&'a ToolPauseRequest> {
    pending_tool_pauses.iter().find(|pause| {
        pause.source_thread_id.as_deref() == Some(node.thread_id.as_str())
            && pause_preview_tool_use_id(pause) == tool_use_id
    })
}

fn tool_pause_label(preview: &ToolPauseRequest) -> &'static str {
    match &preview.kind {
        ToolPauseKind::Permission(_) => "Waiting for permission",
        ToolPauseKind::UserInput(_) => "Waiting for answer",
    }
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
        "ask_user" => "Ask User".to_string(),
        "todo_write" => "Todo List".to_string(),
        "view_image" => "View Image".to_string(),
        "bash" => "Bash".to_string(),
        other => {
            let words = label_words(other);
            if words.is_empty() {
                return other.to_string();
            }

            let mut out = String::new();
            for (i, word) in words.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                push_capitalized(&mut out, word);
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
        "view_image" => tool_use
            .input
            .get("path")
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
    use crate::types::events::{PermissionPreview, ReadPermissionPreview};
    use omini_domain::message::{Message, Role};

    #[test]
    fn rejected_subagent_without_started_event_renders_finished() {
        let tool_use = ToolUseBlock {
            id: "tool_1".to_string(),
            name: "spawn_agent".to_string(),
            input: std::collections::HashMap::from([(
                "name".to_string(),
                serde_json::Value::String("explorer".to_string()),
            )]),
        };
        let result = ToolResultBlock {
            tool_use_id: "tool_1".to_string(),
            is_error: true,
            content: "spawn_agent is not available in plan profile".to_string(),
            metadata: None,
        };

        let lines =
            render_subagent_tool(&tool_use, Some(&result), None, &VecDeque::new(), 80, None);
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert_eq!(lines[0].spans.len(), 2);
        assert!(rendered[0].contains("Explorer"));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("spawn_agent is not available"))
        );
    }

    #[test]
    fn active_child_tool_marker_keeps_tool_label_aligned() {
        let tool_use = ToolUseBlock {
            id: "spawn-1".to_string(),
            name: "spawn_agent".to_string(),
            input: std::collections::HashMap::from([(
                "name".to_string(),
                serde_json::Value::String("explorer".to_string()),
            )]),
        };
        let read_tool = |id: &str, path: &str| ToolUseBlock {
            id: id.to_string(),
            name: "read".to_string(),
            input: std::collections::HashMap::from([(
                "file_path".to_string(),
                serde_json::Value::String(path.to_string()),
            )]),
        };
        let node = SubagentNode {
            task_id: "task-1".to_string(),
            thread_id: "subagent-1".to_string(),
            parent_thread_id: "main".to_string(),
            spawn_tool_use_id: "spawn-1".to_string(),
            agent_label: "explorer".to_string(),
            title: "Explore".to_string(),
            execution_mode: crate::types::events::AgentTaskExecutionMode::Background,
            status: AgentTaskStatus::Running,
            messages: vec![Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::ToolUse(read_tool("read-1", "README.md")),
                    ContentBlock::ToolUse(read_tool("read-2", "Cargo.toml")),
                ],
            )],
        };
        let pending_tool_pauses = VecDeque::from([ToolPauseRequest {
            tool_use_id: "subagent-1:pause-read-2".to_string(),
            preview_tool_use_id: Some("read-2".to_string()),
            tool_name: "read".to_string(),
            permission_source: None,
            source_thread_id: Some("subagent-1".to_string()),
            source_agent_label: Some("Explorer".to_string()),
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: "Cargo.toml".to_string(),
            })),
        }]);

        let lines =
            render_subagent_tool(&tool_use, None, Some(&node), &pending_tool_pauses, 80, None);
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();
        let first_read_prefix = rendered[1].split("Read").next().unwrap().width();
        let active_read_prefix = rendered[2].split("Read").next().unwrap().width();

        assert_eq!(rendered[1], "  └ Read README.md");
        assert!(rendered[2].starts_with("  • Read Cargo.toml"));
        assert_eq!(first_read_prefix, active_read_prefix);
    }

    #[test]
    fn child_view_image_tool_renders_name_and_path() {
        let tool_use = ToolUseBlock {
            id: "spawn-1".to_string(),
            name: "spawn_agent".to_string(),
            input: std::collections::HashMap::from([(
                "name".to_string(),
                serde_json::Value::String("explorer".to_string()),
            )]),
        };
        let node = SubagentNode {
            task_id: "task-1".to_string(),
            thread_id: "subagent-1".to_string(),
            parent_thread_id: "main".to_string(),
            spawn_tool_use_id: "spawn-1".to_string(),
            agent_label: "explorer".to_string(),
            title: "Explore".to_string(),
            execution_mode: crate::types::events::AgentTaskExecutionMode::Background,
            status: AgentTaskStatus::Running,
            messages: vec![Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse(ToolUseBlock {
                    id: "image-1".to_string(),
                    name: "view_image".to_string(),
                    input: std::collections::HashMap::from([(
                        "path".to_string(),
                        serde_json::Value::String("/tmp/image.png".to_string()),
                    )]),
                })],
            )],
        };

        let lines = render_subagent_tool(&tool_use, None, Some(&node), &VecDeque::new(), 80, None);
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert_eq!(rendered[1], "  └ View Image /tmp/image.png");
    }
}
