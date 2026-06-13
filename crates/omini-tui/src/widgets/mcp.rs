use omini_domain::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};
use std::collections::HashMap;

use super::{tool_error_display_text, tool_title_style, truncate_display_width, word_wrap};

const MAX_RESULT_LINES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct McpToolDisplay {
    pub(super) server_name: String,
    pub(super) server_tool_name: String,
}

pub(super) fn is_mcp_tool(tool_use: &ToolUseBlock) -> bool {
    tool_use.name.starts_with("mcp__")
}

pub(super) fn display_info(tool_use: &ToolUseBlock) -> McpToolDisplay {
    info_from_registered_tool_name(&tool_use.name)
}

pub(super) fn title_line(
    tool_use: &ToolUseBlock,
    title_style: Style,
    content_width: usize,
    _pending: bool,
) -> Line<'static> {
    let detail_style = Style::default().fg(Color::Rgb(140, 142, 150));
    let info = display_info(tool_use);
    let mut spans = vec![Span::raw("· "), Span::styled("MCP", title_style)];
    let mut label = format!(" {}/{}", info.server_name, info.server_tool_name);
    let input = compact_json_object(&tool_use.input);
    if input != "{}" {
        label.push(' ');
        label.push_str(&input);
    }
    if !label.is_empty() {
        let used_width: usize = spans.iter().map(|span| span.width()).sum();
        spans.push(Span::styled(
            truncate_display_width(&label, content_width.saturating_sub(used_width)),
            detail_style,
        ));
    }
    Line::from(spans)
}

pub(super) fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let error = Color::Rgb(255, 100, 100);
    let output = Color::Rgb(156, 156, 156);
    let pending = result.is_none();
    let title_style = tool_title_style(accent, pending);

    let mut lines = vec![title_line(tool_use, title_style, content_width, pending)];
    let Some(result) = result else {
        return lines;
    };

    let content = if result.is_error {
        let error_text = tool_error_display_text(&result.content);
        if error_text == result.content.trim() {
            mcp_result_display_text(&result.content)
        } else {
            error_text
        }
    } else {
        mcp_result_display_text(&result.content)
    };
    if content.trim().is_empty() {
        return lines;
    }

    push_result_lines(
        &mut lines,
        content,
        Style::default().fg(if result.is_error { error } else { output }),
        content_width,
        Some(MAX_RESULT_LINES),
    );
    lines
}

fn info_from_registered_tool_name(registered_tool_name: &str) -> McpToolDisplay {
    let Some(rest) = registered_tool_name.strip_prefix("mcp__") else {
        return McpToolDisplay {
            server_name: "mcp".to_string(),
            server_tool_name: registered_tool_name.to_string(),
        };
    };
    let mut parts = rest.splitn(2, "__");
    let server_name = parts.next().unwrap_or("mcp").to_string();
    let server_tool_name = parts.next().unwrap_or(rest).to_string();
    McpToolDisplay {
        server_name,
        server_tool_name,
    }
}

fn compact_json_object(input: &HashMap<String, Value>) -> String {
    let mut sorted = Map::new();
    let mut keys = input.keys().collect::<Vec<_>>();
    keys.sort();
    for key in keys {
        if let Some(value) = input.get(key) {
            sorted.insert(key.clone(), sorted_json_value(value));
        }
    }
    serde_json::to_string(&Value::Object(sorted)).unwrap_or_else(|_| "{}".to_string())
}

fn sorted_json_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut sorted = Map::new();
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if let Some(value) = object.get(key) {
                    sorted.insert(key.clone(), sorted_json_value(value));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(sorted_json_value).collect()),
        _ => value.clone(),
    }
}

fn mcp_result_display_text(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return trimmed.to_string();
    };

    if let Some(text) = mcp_content_text(&value)
        && !text.trim().is_empty()
    {
        return text;
    }

    if let Some(value) = value
        .get("structuredContent")
        .or_else(|| value.get("structured_content"))
    {
        return pretty_json(value);
    }

    pretty_json(&value)
}

fn mcp_content_text(value: &Value) -> Option<String> {
    let content = value.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = item.get("text").and_then(Value::as_str)
        {
            parts.push(text.to_string());
            continue;
        }
        parts.push(pretty_json(item));
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn push_result_lines(
    lines: &mut Vec<Line<'static>>,
    content: String,
    style: Style,
    content_width: usize,
    max_lines: Option<usize>,
) {
    let first_prefix = "  └ ";
    let continuation_prefix = "    ";
    let wrap_width = content_width.saturating_sub(first_prefix.len()).max(1);
    let wrapped = word_wrap(&content, wrap_width);
    let limit = max_lines.unwrap_or(wrapped.len());
    let shown = wrapped.len().min(limit);
    for (idx, line) in wrapped.iter().take(shown).enumerate() {
        let prefix = if idx == 0 {
            first_prefix
        } else {
            continuation_prefix
        };
        lines.push(Line::from(vec![
            Span::raw(prefix),
            Span::styled(line.clone(), style),
        ]));
    }
    if wrapped.len() > shown {
        let omitted = wrapped.len().saturating_sub(shown);
        lines.push(Line::from(vec![
            Span::raw(continuation_prefix),
            Span::styled(
                format!("... {omitted} lines omitted ..."),
                Style::default()
                    .fg(Color::Rgb(140, 142, 150))
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    }
}
