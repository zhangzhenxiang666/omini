use crate::types::events::ToolPauseRequest;
use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

mod ask_user;
mod bash;
mod file_mutation;
mod read;
mod search;
mod skill;

pub fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return text.lines().map(|l| l.to_string()).collect();
    }

    let mut result = Vec::new();

    for line in text.split('\n') {
        let line_width = UnicodeWidthStr::width(line);
        if line_width <= max_width {
            result.push(line.to_string());
            continue;
        }

        let mut start = 0;
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();

        while start < len {
            let mut end = start;
            let mut w = 0;
            while end < len {
                let cw = UnicodeWidthChar::width(chars[end]).unwrap_or(0);
                if w + cw > max_width {
                    break;
                }
                w += cw;
                end += 1;
            }

            if end == start {
                end = start + 1;
            } else if end < len && !chars[end].is_whitespace() {
                let mut break_at = end;
                while break_at > start && !chars[break_at - 1].is_whitespace() {
                    break_at -= 1;
                }
                if break_at > start {
                    end = break_at;
                }
            }

            let segment: String = chars[start..end].iter().collect();
            result.push(segment.trim_end().to_string());

            start = end;
            while start < len && chars[start].is_whitespace() {
                start += 1;
            }
        }
    }

    result
}

/// 基于时间的 spinner 字符（每 80ms 切换一帧）。
fn spinner() -> &'static str {
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let idx = (ms / 80) as usize % frames.len();
    frames[idx]
}

pub fn build_bordered_lines(
    text: &str,
    content_width: usize,
    border_color: Color,
    italic: bool,
    bg: Option<Color>,
) -> Vec<Line<'static>> {
    let available = content_width.max(1);
    let mut lines: Vec<Line> = Vec::new();

    let mut content_style = Style::default().fg(border_color);
    if italic {
        content_style = content_style.add_modifier(Modifier::ITALIC);
    }
    if let Some(c) = bg {
        content_style = content_style.bg(c);
    }

    let wrapped = word_wrap(text, available);
    if wrapped.is_empty() {
        lines.push(Line::from(Span::styled(String::new(), content_style)));
    } else {
        for wrapped_line in wrapped {
            lines.push(Line::from(Span::styled(wrapped_line, content_style)));
        }
    }

    lines
}

pub fn tool_error_display_text(content: &str) -> String {
    if let Some(text) = permission_denied_display_text(content) {
        return text;
    }

    content.trim().to_string()
}

fn permission_denied_display_text(content: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(content.trim()).ok()?;
    let object = value.as_object()?;
    if object.get("error").and_then(|value| value.as_str()) != Some("permission_denied") {
        return None;
    }

    let guidance = object
        .get("user_guidance")
        .and_then(|value| value.as_str())
        .map(collapse_whitespace)
        .filter(|value| !value.is_empty());

    Some(match guidance {
        Some(guidance) => format!("Permission denied · {guidance}"),
        None => "Permission denied".to_string(),
    })
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn display_path(path: &str, project_dir: Option<&Path>) -> String {
    let path_obj = Path::new(path);

    if let Some(project_dir) = project_dir.filter(|p| !p.as_os_str().is_empty())
        && let Ok(relative) = path_obj.strip_prefix(project_dir)
    {
        return if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            relative.display().to_string()
        };
    }

    if let Some(home_dir) = dirs::home_dir().filter(|p| !p.as_os_str().is_empty())
        && let Ok(relative) = path_obj.strip_prefix(&home_dir)
    {
        return if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.display())
        };
    }

    path.to_string()
}

pub fn build_thinking_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    let available = content_width.saturating_sub(2);
    let mut lines: Vec<Line> = Vec::new();

    let border_style = Style::default().fg(Color::DarkGray);
    let prefix_style = Style::default()
        .fg(Color::Rgb(141, 119, 78))
        .add_modifier(Modifier::ITALIC);
    let text_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);
    let rail_spans = || vec![Span::styled("\u{2503}", border_style), Span::raw(" ")];

    if text.is_empty() {
        let mut spans = rail_spans();
        spans.push(Span::styled("Thinking: ", prefix_style));
        lines.push(Line::from(spans));
        return lines;
    }

    let prefix = "Thinking: ";
    let prefix_w = UnicodeWidthStr::width(prefix);
    let first_line_available = available.saturating_sub(prefix_w);
    let logical_lines: Vec<&str> = text.split('\n').collect();

    for (ll_idx, ll) in logical_lines.iter().enumerate() {
        let is_first = ll_idx == 0;

        if is_first && first_line_available == 0 {
            let mut spans = rail_spans();
            spans.push(Span::styled(prefix, prefix_style));
            lines.push(Line::from(spans));
            let wrapped = word_wrap(ll, available);
            for wl in wrapped {
                let mut spans = rail_spans();
                spans.push(Span::styled(wl, text_style));
                lines.push(Line::from(spans));
            }
            continue;
        }

        if ll.is_empty() {
            if is_first {
                let mut spans = rail_spans();
                spans.push(Span::styled(prefix, prefix_style));
                lines.push(Line::from(spans));
            } else {
                lines.push(Line::from(rail_spans()));
            }
            continue;
        }

        if is_first {
            let first_w = UnicodeWidthStr::width(*ll);
            if prefix_w + first_w <= available {
                let mut spans = rail_spans();
                spans.push(Span::styled(prefix, prefix_style));
                spans.push(Span::styled(ll.to_string(), text_style));
                lines.push(Line::from(spans));
            } else {
                let first_wrapped = word_wrap(ll, first_line_available);
                let first_chunk = first_wrapped.first().cloned().unwrap_or_default();
                let mut spans = rail_spans();
                spans.push(Span::styled(prefix, prefix_style));
                spans.push(Span::styled(first_chunk, text_style));
                lines.push(Line::from(spans));
                if first_wrapped.len() > 1 {
                    let rest = first_wrapped[1..].join(" ");
                    let rest_wrapped = word_wrap(&rest, available);
                    for rl in rest_wrapped {
                        let mut spans = rail_spans();
                        spans.push(Span::styled(rl, text_style));
                        lines.push(Line::from(spans));
                    }
                }
            }
        } else {
            let wrapped = word_wrap(ll, available);
            for wl in wrapped {
                let mut spans = rail_spans();
                spans.push(Span::styled(wl, text_style));
                lines.push(Line::from(spans));
            }
        }
    }

    lines
}

pub fn render_tool(
    tool_use: &ToolUseBlock,
    tool_result: Option<&ToolResultBlock>,
    tool_preview: Option<&ToolPauseRequest>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    match tool_use.name.as_str() {
        "bash" => bash::render(tool_use, tool_result, content_width),
        "search" => search::render(tool_use, tool_result, content_width, project_dir),
        "read" => read::render(
            tool_use,
            tool_result,
            tool_preview,
            content_width,
            project_dir,
        ),
        "skill" => skill::render(tool_use, tool_result, content_width),
        "edit" => file_mutation::render_edit(
            tool_use,
            tool_result,
            tool_preview,
            content_width,
            project_dir,
        ),
        "write" => file_mutation::render_write(
            tool_use,
            tool_result,
            tool_preview,
            content_width,
            project_dir,
        ),
        "ask_user" => ask_user::render(tool_use, tool_result, content_width),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn bordered_lines_render_without_left_rail() {
        let color = Color::Rgb(100, 200, 130);
        let lines = build_bordered_lines("tool result", 40, color, false, None);

        assert_eq!(plain(&lines[0]), "tool result");
        assert_eq!(lines[0].spans[0].style.fg, Some(color));
    }

    #[test]
    fn thinking_lines_render_with_left_rail() {
        let lines = build_thinking_lines("checking context", 40);

        assert_eq!(plain(&lines[0]), "\u{2503} Thinking: checking context");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn skill_tool_renders_invoked_skill_command() {
        let mut input = std::collections::HashMap::new();
        input.insert("name".to_string(), serde_json::json!("commit-message"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "skill".to_string(),
            input,
        };
        let tool_result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: false,
            content: String::new(),
            metadata: None,
        };

        let lines = render_tool(&tool_use, Some(&tool_result), None, 80, None);

        assert_eq!(plain(&lines[0]), "· Skill commit-message");
    }

    #[test]
    fn bash_tool_error_is_rendered_as_error_not_output() {
        let mut input = std::collections::HashMap::new();
        input.insert(
            "command".to_string(),
            serde_json::json!("git commit -m 'feat: add skills system'"),
        );
        input.insert("description".to_string(), serde_json::json!("创建提交"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "bash".to_string(),
            input,
        };
        let tool_result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: true,
            content: "Permission denied for tool: bash".to_string(),
            metadata: None,
        };

        let lines = render_tool(&tool_use, Some(&tool_result), None, 80, None);

        assert!(plain(&lines[0]).starts_with("· Bash("));
        assert_eq!(plain(&lines[1]), "  └─ # 创建提交");
        assert_eq!(plain(&lines[2]), "  Permission denied for tool: bash");
        assert_eq!(
            lines[0].spans[1].style.fg,
            Some(Color::Rgb(0x42, 0xb3, 0xc2))
        );
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Rgb(255, 100, 100)));
    }

    #[test]
    fn permission_denied_json_displays_as_guidance_summary() {
        let content = serde_json::json!({
            "error": "permission_denied",
            "message": "Permission denied for tool: write",
            "user_guidance": "Use English comments.\nAvoid extra changes.",
            "required_action": "retry_with_user_guidance",
        })
        .to_string();

        assert_eq!(
            tool_error_display_text(&content),
            "Permission denied · Use English comments. Avoid extra changes."
        );
    }

    #[test]
    fn bash_permission_denied_json_does_not_render_raw_json() {
        let mut input = std::collections::HashMap::new();
        input.insert("command".to_string(), serde_json::json!("touch demo"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "bash".to_string(),
            input,
        };
        let tool_result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: true,
            content: serde_json::json!({
                "error": "permission_denied",
                "message": "Permission denied for tool: bash",
                "user_guidance": "Inspect first",
                "required_action": "retry_with_user_guidance",
            })
            .to_string(),
            metadata: None,
        };

        let lines = render_tool(&tool_use, Some(&tool_result), None, 80, None);

        assert_eq!(plain(&lines[1]), "  Permission denied · Inspect first");
        assert!(!plain(&lines[1]).contains("required_action"));
    }
}
