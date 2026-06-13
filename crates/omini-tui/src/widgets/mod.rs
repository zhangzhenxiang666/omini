use crate::types::events::{ToolPauseKind, ToolPauseRequest};
use omini_domain::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Map;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

mod ask_user;
mod bash;
pub(crate) mod bash_highlight;
mod file_mutation;
mod mcp;
mod read;
mod search;
mod skill;
mod todo_write;

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
// TODO: 当前 pending 状态改用标题文字呼吸；保留此函数，后续需要独立 spinner 时复用。
#[allow(dead_code)]
fn spinner() -> &'static str {
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let idx = (ms / 80) as usize % frames.len();
    frames[idx]
}

/// 工具标题样式。
///
/// pending 工具通过标题颜色呼吸来表达加载状态，不再追加 spinner 字符，
/// 以避免不同终端字体下符号基线不一致的问题。
pub(crate) fn tool_title_style(color: Color, pending: bool) -> Style {
    let color = if pending {
        breathing_color(color)
    } else {
        color
    };
    Style::default().fg(color)
}

fn breathing_color(color: Color) -> Color {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let phase = (ms % 1600) as f64 / 1600.0;
    breathing_color_at(color, phase)
}

fn breathing_color_at(color: Color, phase: f64) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    let phase = phase.rem_euclid(1.0);
    let breath = 0.5 - 0.5 * (phase * std::f64::consts::TAU).cos();
    let scale = 0.65 + breath * 0.57;
    Color::Rgb(
        scale_channel(r, scale),
        scale_channel(g, scale),
        scale_channel(b, scale),
    )
}

fn scale_channel(value: u8, scale: f64) -> u8 {
    ((value as f64 * scale).round()).clamp(0.0, 255.0) as u8
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
    tool_pause_active: Option<bool>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    if tool_result.is_none()
        && let (Some(preview), Some(tool_pause_active)) = (tool_preview, tool_pause_active)
    {
        let mut lines =
            compact_waiting_tool_lines(tool_use, tool_pause_active, content_width, project_dir);
        decorate_paused_tool(&mut lines, preview, tool_pause_active);
        return lines;
    }

    let mut lines = if mcp::is_mcp_tool(tool_use) {
        mcp::render(tool_use, tool_result, content_width)
    } else {
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
            "view_image" => read::render_view_image(
                tool_use,
                tool_result,
                tool_preview,
                content_width,
                project_dir,
            ),
            "skill" => skill::render(tool_use, tool_result, content_width),
            "todo_write" => todo_write::render(tool_use, tool_result, content_width),
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
    };

    if lines.is_empty() && tool_preview.is_some() {
        lines.push(Line::from(vec![
            Span::raw("· "),
            Span::styled(
                tool_use.name.clone(),
                tool_title_style(Color::Rgb(0x42, 0xb3, 0xc2), tool_result.is_none()),
            ),
        ]));
    }

    if tool_result.is_none()
        && let (Some(preview), Some(tool_pause_active)) = (tool_preview, tool_pause_active)
    {
        decorate_paused_tool(&mut lines, preview, tool_pause_active);
    }

    lines
}

fn compact_waiting_tool_lines(
    tool_use: &ToolUseBlock,
    tool_pause_active: bool,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let title_style = tool_title_style(accent, !tool_pause_active);
    if mcp::is_mcp_tool(tool_use) {
        return vec![mcp::title_line(
            tool_use,
            title_style,
            content_width,
            !tool_pause_active,
        )];
    }
    let mut spans = vec![Span::raw("· ")];

    match tool_use.name.as_str() {
        "read" | "view_image" | "edit" | "write" => {
            let title = match tool_use.name.as_str() {
                "read" => "Read",
                "view_image" => "View Image",
                "edit" => "Edit",
                "write" => "Write",
                _ => unreachable!(),
            };
            spans.push(Span::styled(title, title_style));
            spans.push(Span::raw(format!(
                " {}",
                compact_tool_path(tool_use, project_dir)
            )));
        }
        "bash" => {
            spans.push(Span::styled("Command", title_style));
            let command = tool_use
                .input
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            let used_width: usize = spans.iter().map(|span| span.width()).sum();
            let command_width = content_width
                .saturating_sub(used_width)
                .saturating_sub(UnicodeWidthStr::width("()"));
            spans.push(Span::raw("("));
            spans.extend(bash_highlight::truncated_command_spans(
                command,
                command_width,
                Style::default().fg(bash_highlight::COMMAND_TEXT_FG),
            ));
            spans.push(Span::raw(")"));
        }
        "ask_user" => {
            spans.push(Span::styled("Ask User", title_style));
            let count = tool_use
                .input
                .get("questions")
                .and_then(|value| value.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            spans.push(Span::raw(format!(
                " ({} question{})",
                count,
                if count == 1 { "" } else { "s" }
            )));
        }
        other => {
            spans.push(Span::styled(other.to_string(), title_style));
        }
    }

    vec![Line::from(spans)]
}

fn compact_tool_path(tool_use: &ToolUseBlock, project_dir: Option<&Path>) -> String {
    let path_key = if tool_use.name == "view_image" {
        "path"
    } else {
        "file_path"
    };
    let path = tool_use
        .input
        .get(path_key)
        .and_then(|value| value.as_str())
        .unwrap_or("<unknown>");
    display_path(path, project_dir)
}

fn decorate_paused_tool(
    lines: &mut Vec<Line<'static>>,
    preview: &ToolPauseRequest,
    tool_pause_active: bool,
) {
    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let dim = Color::Rgb(140, 145, 155);
    if tool_pause_active && let Some(first) = lines.first_mut() {
        let active_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
        if first
            .spans
            .first()
            .is_some_and(|span| span.content.as_ref() == "· ")
        {
            first.spans[0] = Span::styled("• ", active_style);
        } else {
            first.spans.insert(0, Span::styled("• ", active_style));
        }
    }

    let status_style = if tool_pause_active {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    lines.push(Line::from(vec![
        Span::raw("  └ "),
        Span::styled(tool_pause_label(preview), status_style),
    ]));
}

pub(crate) fn truncate_display_width(s: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let ellipsis = "...";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ellipsis.chars().take(max_width).collect();
    }

    let target = max_width - ellipsis_width;
    let mut result = String::new();
    let mut current_width = 0;
    for ch in s.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > target {
            break;
        }
        result.push(ch);
        current_width += ch_width;
    }
    result.push_str(ellipsis);
    result
}

fn tool_pause_label(preview: &ToolPauseRequest) -> &'static str {
    match &preview.kind {
        ToolPauseKind::Permission(_) => "Waiting for permission",
        ToolPauseKind::UserInput(_) => "Waiting for answer",
    }
}

pub fn preview_placeholder_result(tool_use: &ToolUseBlock) -> ToolResultBlock {
    ToolResultBlock {
        tool_use_id: tool_use.id.clone(),
        is_error: false,
        content: String::new(),
        metadata: Some(Map::new()),
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
    fn breathing_color_pulses_rgb_and_preserves_other_colors() {
        assert_eq!(
            breathing_color_at(Color::Rgb(100, 150, 200), 0.0),
            Color::Rgb(65, 98, 130)
        );
        assert_eq!(
            breathing_color_at(Color::Rgb(100, 150, 200), 0.5),
            Color::Rgb(122, 183, 244)
        );
        assert_eq!(breathing_color_at(Color::DarkGray, 0.5), Color::DarkGray);
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

        let lines = render_tool(&tool_use, Some(&tool_result), None, None, 80, None);

        assert_eq!(plain(&lines[0]), "· Skill commit-message");
    }

    #[test]
    fn view_image_tool_renders_like_read_with_view_image_title() {
        let mut input = std::collections::HashMap::new();
        input.insert("path".to_string(), serde_json::json!("/tmp/image.png"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "view_image".to_string(),
            input,
        };
        let tool_result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: false,
            content: "Loaded image: /tmp/image.png".to_string(),
            metadata: None,
        };

        let lines = render_tool(&tool_use, Some(&tool_result), None, None, 80, None);

        assert_eq!(lines.len(), 1);
        assert_eq!(plain(&lines[0]), "· View Image /tmp/image.png");
    }

    #[test]
    fn view_image_tool_error_aligns_like_read_error() {
        let mut input = std::collections::HashMap::new();
        input.insert("path".to_string(), serde_json::json!("/tmp/image.png"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "view_image".to_string(),
            input,
        };
        let tool_result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: true,
            content: "Failed to read image /tmp/image.png".to_string(),
            metadata: None,
        };

        let lines = render_tool(&tool_use, Some(&tool_result), None, None, 80, None);

        assert_eq!(plain(&lines[0]), "· View Image /tmp/image.png");
        assert_eq!(plain(&lines[1]), "  Failed to read image /tmp/image.png");
        assert_eq!(lines[1].spans[1].style.fg, Some(Color::Rgb(255, 100, 100)));
    }

    #[test]
    fn paused_view_image_tool_renders_name_and_path() {
        let mut input = std::collections::HashMap::new();
        input.insert("path".to_string(), serde_json::json!("/tmp/image.png"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "view_image".to_string(),
            input,
        };
        let preview = ToolPauseRequest {
            tool_use_id: "toolu_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "view_image".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(crate::types::events::PermissionPreview::Read(
                crate::types::events::ReadPermissionPreview {
                    file_path: "/tmp/image.png".to_string(),
                },
            )),
        };

        let lines = render_tool(&tool_use, None, Some(&preview), Some(false), 80, None);

        assert_eq!(plain(&lines[0]), "· View Image /tmp/image.png");
        assert_eq!(plain(&lines[1]), "  └ Waiting for permission");
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

        let lines = render_tool(&tool_use, Some(&tool_result), None, None, 80, None);

        assert!(plain(&lines[0]).starts_with("· Command("));
        assert_eq!(plain(&lines[1]), "  └ # 创建提交");
        assert_eq!(plain(&lines[2]), "  Permission denied for tool: bash");
        assert_eq!(
            lines[0].spans[1].style.fg,
            Some(Color::Rgb(0x42, 0xb3, 0xc2))
        );
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Rgb(255, 100, 100)));
    }

    #[test]
    fn mcp_tool_renders_service_tool_input_and_text_result() {
        let mut input = std::collections::HashMap::new();
        input.insert("query".to_string(), serde_json::json!("rust"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "mcp__docs__search".to_string(),
            input,
        };
        let tool_result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: false,
            content: serde_json::json!({
                "content": [{"type": "text", "text": "found docs"}]
            })
            .to_string(),
            metadata: None,
        };

        let lines = render_tool(&tool_use, Some(&tool_result), None, None, 80, None);
        let rendered: Vec<_> = lines.iter().map(plain).collect();

        assert_eq!(rendered[0], "· MCP docs/search {\"query\":\"rust\"}");
        assert_eq!(rendered[1], "  └ found docs");
    }

    #[test]
    fn pending_mcp_tool_breathes_across_call_summary() {
        let mut input = std::collections::HashMap::new();
        input.insert("query".to_string(), serde_json::json!("rust"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "mcp__docs__search".to_string(),
            input,
        };

        let lines = render_tool(&tool_use, None, None, None, 80, None);

        assert_eq!(plain(&lines[0]), "· MCP docs/search {\"query\":\"rust\"}");
        assert_eq!(lines[0].spans[1].content.as_ref(), "MCP");
        assert_eq!(lines[0].spans[2].style.fg, Some(Color::Rgb(140, 142, 150)));
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

        let lines = render_tool(&tool_use, Some(&tool_result), None, None, 80, None);

        assert_eq!(plain(&lines[1]), "  Permission denied · Inspect first");
        assert!(!plain(&lines[1]).contains("required_action"));
    }

    #[test]
    fn todo_write_renders_status_checklist_without_json() {
        let mut input = std::collections::HashMap::new();
        input.insert(
            "todos".to_string(),
            serde_json::json!([
                {"content": "Read existing flow", "status": "completed"},
                {"content": "Add UpdateTodo widget", "status": "in_progress"},
                {"content": "Run focused tests", "status": "pending"},
                {"content": "Drop obsolete step", "status": "cancelled"}
            ]),
        );
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "todo_write".to_string(),
            input,
        };
        let tool_result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: false,
            content: serde_json::json!({
                "todos": [
                    {"content": "Read existing flow", "status": "completed"}
                ]
            })
            .to_string(),
            metadata: None,
        };

        let lines = render_tool(&tool_use, Some(&tool_result), None, None, 80, None);
        let rendered: Vec<_> = lines.iter().map(plain).collect();

        assert_eq!(rendered[0], "· Todo List");
        assert_eq!(rendered[1], "  └ ✔ Read existing flow");
        assert_eq!(rendered[2], "    □ Add UpdateTodo widget");
        assert_eq!(rendered[3], "    □ Run focused tests");
        assert_eq!(rendered[4], "    ✘ Drop obsolete step");
        assert!(rendered.iter().all(|line| !line.contains("\"todos\"")));
        assert_eq!(
            lines[2].spans[3].style.fg,
            Some(Color::Rgb(0x42, 0xb3, 0xc2))
        );
        assert_eq!(lines[2].spans[1].style.fg, Some(Color::Rgb(170, 174, 184)));
        assert!(
            !lines[4].spans[1]
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
        assert!(
            lines[4].spans[3]
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
    }
}
