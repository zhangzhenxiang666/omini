use crate::app::event::ToolPauseRequest;
use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Map;
use std::path::Path;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

mod ask_user;
mod bash;
pub mod bash_highlight;
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

/// 标题只依赖工具类型，避免计时动画使历史条目持续失效。
pub fn tool_title_style(color: Color) -> Style {
    Style::default().fg(color)
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

pub fn format_thinking_duration(duration_ms: u64) -> String {
    if duration_ms < 1000 {
        return "<1s".to_string();
    }
    // 不足 1 秒的分量向下取整，零分量省略（"1m" 而非 "1m 0s"）
    let total_secs = duration_ms / 1000;
    let seconds = total_secs % 60;
    let minutes = (total_secs / 60) % 60;
    let hours = total_secs / 3600;
    if hours > 0 {
        if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        }
    } else if minutes > 0 {
        if seconds > 0 {
            format!("{minutes}m {seconds}s")
        } else {
            format!("{minutes}m")
        }
    } else {
        format!("{seconds}s")
    }
}

/// 参与回合活动聚合的工具类别；决定摘要行里的统计短语。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Shell,
    FileRead,
    FileEdit,
    FileWrite,
    Search,
    Skill,
    McpTool,
    Other(String),
}

pub fn tool_category(tool_use: &ToolUseBlock) -> ToolCategory {
    if mcp::is_mcp_tool(tool_use) {
        return ToolCategory::McpTool;
    }
    match tool_use.name.as_str() {
        "bash" => ToolCategory::Shell,
        "read" => ToolCategory::FileRead,
        "edit" => ToolCategory::FileEdit,
        "write" => ToolCategory::FileWrite,
        "search" => ToolCategory::Search,
        "skill" => ToolCategory::Skill,
        other => ToolCategory::Other(other.to_string()),
    }
}

/// 回合活动摘要行：思考时长 + 工具统计聚合为一条，如
/// `Thought for 12s, ran 1 shell command, read 1 file`。
/// `thinking_ms` 为 None 时不输出思考部分（旧记录或非思考模型）。
pub fn activity_summary_line(
    thinking_ms: Option<u64>,
    tool_counts: &[(ToolCategory, usize)],
    content_width: usize,
) -> Option<Line<'static>> {
    if thinking_ms.is_some_and(|ms| ms < 1_000) && tool_counts.is_empty() {
        return None;
    }

    let mut phrases: Vec<String> = Vec::new();
    if let Some(ms) = thinking_ms {
        phrases.push(format!("Thought for {}", format_thinking_duration(ms)));
    }
    for (category, count) in tool_counts {
        phrases.push(category_phrase(category, *count));
    }
    if phrases.is_empty() {
        return None;
    }

    // 无思考部分时首短语首字母大写，作为摘要行开头
    if thinking_ms.is_none()
        && let Some(first) = phrases.first_mut()
    {
        let mut chars = first.chars();
        if let Some(head) = chars.next() {
            *first = format!("{}{}", head.to_uppercase(), chars.as_str());
        }
    }

    let style = Style::default()
        .fg(crate::ui::theme::MUTED)
        .add_modifier(Modifier::ITALIC);
    let text = phrases.join(", ");
    Some(Line::from(Span::styled(
        truncate_display_width(
            &format!(
                "{}{}",
                if thinking_ms.is_some() { "  " } else { "⏺ " },
                text
            ),
            content_width,
        ),
        style,
    )))
}

fn category_phrase(category: &ToolCategory, count: usize) -> String {
    let plural = if count == 1 { "" } else { "s" };
    match category {
        ToolCategory::Shell => format!("ran {count} shell command{plural}"),
        ToolCategory::FileRead => format!("read {count} file{plural}"),
        ToolCategory::FileEdit => format!("edited {count} file{plural}"),
        ToolCategory::FileWrite => format!("wrote {count} file{plural}"),
        ToolCategory::Search => format!("ran {count} search{}", if count == 1 { "" } else { "es" }),
        ToolCategory::Skill => format!("used {count} skill{plural}"),
        ToolCategory::McpTool => format!("called {count} MCP tool{plural}"),
        ToolCategory::Other(name) => format!("used {name} ×{count}"),
    }
}

/// 纯编排调用不产生可见条目，也不参与活动统计；`send_message` 单独作为分界展示。
pub fn is_hidden_tool(name: &str) -> bool {
    matches!(name, "read_task" | "cancel_task")
}

pub fn render_tool(
    tool_use: &ToolUseBlock,
    tool_result: Option<&ToolResultBlock>,
    tool_preview: Option<&ToolPauseRequest>,
    tool_pause_active: Option<bool>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    if is_hidden_tool(&tool_use.name) {
        return Vec::new();
    }
    if tool_result.is_none()
        && let (Some(_), Some(tool_pause_active)) = (tool_preview, tool_pause_active)
    {
        let mut lines = compact_waiting_tool_lines(tool_use, content_width, project_dir);
        decorate_paused_tool(&mut lines, tool_pause_active);
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
            "spawn_agent" | "run_agent" => agent::render(tool_use, tool_result, content_width),
            "send_message" => {
                agent::render_send_message(tool_use, tool_result, None, content_width)
            }
            _ => fallback::render(tool_use, tool_result, content_width),
        }
    };

    if lines.is_empty() && tool_preview.is_some() {
        lines.push(Line::from(vec![
            Span::raw("⏺ "),
            Span::styled(
                tool_use.name.clone(),
                tool_title_style(crate::ui::theme::ACCENT),
            ),
        ]));
    }

    if tool_result.is_none()
        && let (Some(_), Some(tool_pause_active)) = (tool_preview, tool_pause_active)
    {
        decorate_paused_tool(&mut lines, tool_pause_active);
    }

    lines
}

/// 活动摘要下只展示最新工具的输入提示；Shell 和 Search 使用紧凑的专属样式。
/// 行数按工具分别限制，避免长命令或 MCP 参数占满消息视口。
pub fn render_activity_preview(
    tool_use: &ToolUseBlock,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let inner_width = content_width.saturating_sub(4);
    let (lines, max_lines) = if mcp::is_mcp_tool(tool_use) {
        (
            render_tool(tool_use, None, None, None, inner_width, project_dir),
            3,
        )
    } else {
        match tool_use.name.as_str() {
            "bash" => (bash::render_preview(tool_use, inner_width), 2),
            "search" => (
                search::render_preview(tool_use, inner_width, project_dir),
                1,
            ),
            "read" | "view_image" | "skill" => (
                render_tool(tool_use, None, None, None, inner_width, project_dir),
                1,
            ),
            _ => (
                render_tool(tool_use, None, None, None, inner_width, project_dir),
                3,
            ),
        }
    };
    lines
        .into_iter()
        .take(max_lines)
        .enumerate()
        .map(|(index, line)| indent_activity_preview(line, index == 0, content_width))
        .collect()
}

fn indent_activity_preview(
    line: Line<'static>,
    first: bool,
    content_width: usize,
) -> Line<'static> {
    let prefix = if first { "  └ " } else { "    " };
    let prefix_width = UnicodeWidthStr::width(prefix);
    if content_width < prefix_width {
        return Line::from(Span::styled(
            truncate_display_width(prefix, content_width),
            Style::default().fg(crate::ui::theme::MUTED),
        ));
    }

    let mut spans = vec![Span::styled(
        prefix,
        Style::default().fg(crate::ui::theme::MUTED),
    )];
    let mut remaining = content_width - prefix_width;
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let width = span.width();
        if width <= remaining {
            remaining -= width;
            spans.push(span);
        } else {
            spans.push(Span::styled(
                truncate_display_width(span.content.as_ref(), remaining),
                span.style,
            ));
            break;
        }
    }
    Line::from(spans).style(line.style)
}

fn compact_waiting_tool_lines(
    tool_use: &ToolUseBlock,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    if tool_use.name == "bash" {
        return bash::render_preview(tool_use, content_width);
    }
    if tool_use.name == "search" {
        return search::render_preview(tool_use, content_width, project_dir);
    }
    let accent = crate::ui::theme::ACCENT;
    let title_style = tool_title_style(accent);
    if mcp::is_mcp_tool(tool_use) {
        return vec![mcp::title_line(tool_use, title_style, content_width)];
    }
    let mut spans = vec![Span::raw("⏺ ")];

    match tool_use.name.as_str() {
        "read" | "view_image" | "edit" | "write" => {
            let title = match tool_use.name.as_str() {
                "read" => "Read",
                "view_image" => "View Image",
                "edit" => "Edit",
                "write" => "Write",
                _ => unreachable!(),
            };
            let mut path_text = compact_tool_path(tool_use, project_dir);
            if tool_use.name.as_str() == "read" {
                let limit = tool_use.input.get("limit").and_then(|v| v.as_u64());
                let offset = tool_use.input.get("offset").and_then(|v| v.as_u64());
                match (limit, offset) {
                    (Some(l), Some(o)) => {
                        path_text.push_str(&format!(" [offset\u{003d}{o}, limit\u{003d}{l}]"))
                    }
                    (Some(l), None) => path_text.push_str(&format!(" [limit\u{003d}{l}]")),
                    (None, Some(o)) => path_text.push_str(&format!(" [offset\u{003d}{o}]")),
                    (None, None) => {}
                }
            }
            spans.push(Span::styled(title, title_style));
            spans.push(Span::raw(format!(" {path_text}")));
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

fn decorate_paused_tool(lines: &mut Vec<Line<'static>>, tool_pause_active: bool) {
    let accent = crate::ui::theme::ACCENT;
    if tool_pause_active && let Some(first) = lines.first_mut() {
        let active_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
        if first
            .spans
            .first()
            .is_some_and(|span| span.content.as_ref() == "⏺ ")
        {
            first.spans[0] = Span::styled("• ", active_style);
        } else {
            first.spans.insert(0, Span::styled("• ", active_style));
        }
    }
}

pub fn truncate_display_width(s: &str, max_width: usize) -> String {
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
    use crate::app::event::*;

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn bordered_lines_render_without_left_rail() {
        let color = crate::ui::theme::SUCCESS;
        let lines = build_bordered_lines("tool result", 40, color, false, None);

        assert_eq!(plain(&lines[0]), "tool result");
        assert_eq!(lines[0].spans[0].style.fg, Some(color));
    }

    #[test]
    fn format_thinking_duration_covers_subsecond_to_hours() {
        assert_eq!(format_thinking_duration(0), "<1s");
        assert_eq!(format_thinking_duration(999), "<1s");
        assert_eq!(format_thinking_duration(1000), "1s");
        assert_eq!(format_thinking_duration(5300), "5s");
        assert_eq!(format_thinking_duration(59_999), "59s");
        assert_eq!(format_thinking_duration(60_000), "1m");
        assert_eq!(format_thinking_duration(80_000), "1m 20s");
        assert_eq!(format_thinking_duration(3_900_000), "1h 5m");
    }

    #[test]
    fn activity_summary_combines_thinking_and_tool_counts() {
        let counts = vec![(ToolCategory::Shell, 2), (ToolCategory::FileRead, 1)];
        let line = activity_summary_line(Some(12_000), &counts, 80).expect("summary line");

        assert_eq!(
            plain(&line),
            "  Thought for 12s, ran 2 shell commands, read 1 file"
        );
    }

    #[test]
    fn activity_summary_without_thinking_capitalizes_first_phrase() {
        let counts = vec![(ToolCategory::Shell, 1)];
        let line = activity_summary_line(None, &counts, 80).expect("summary line");

        assert_eq!(plain(&line), "⏺ Ran 1 shell command");
    }

    #[test]
    fn activity_summary_empty_renders_nothing() {
        assert!(activity_summary_line(None, &[], 80).is_none());
        // 旧记录：thinking 无时长且无工具 → 不渲染
        assert!(activity_summary_line(None, &[], 80).is_none());
        // 极短的纯思考不占一行；有工具活动时仍保留 Thought 时长摘要。
        assert!(activity_summary_line(Some(999), &[], 80).is_none());
        let counts = vec![(ToolCategory::Shell, 1)];
        let line = activity_summary_line(Some(999), &counts, 80).expect("tool summary");
        assert_eq!(plain(&line), "  Thought for <1s, ran 1 shell command");
    }

    /// 给定普通工具的不同输入；当生成活动附件时，则保留各工具的标识并限制宽度和高度。
    #[test]
    fn renders_tool_previews() {
        let cases = [
            ("bash", "command", "cargo check", "$ cargo check", 1),
            ("search", "query", "AgentRun", "⌕ Search", 1),
            ("read", "file_path", "src/lib.rs", "▤ Read", 1),
            ("skill", "name", "review", "/ Skill", 1),
            ("mcp__files__list", "path", "src", "files/list", 3),
        ];
        for (name, input_key, value, label, max_lines) in cases {
            let tool = ToolUseBlock {
                id: name.into(),
                name: name.into(),
                input: [(input_key.into(), serde_json::json!(value))]
                    .into_iter()
                    .collect(),
            };
            let lines = render_activity_preview(&tool, 24, None);
            let rendered = lines.iter().map(plain).collect::<Vec<_>>().join("\n");
            assert!(rendered.contains(label), "{name}: {rendered}");
            assert!(lines.len() <= max_lines, "{name}: {rendered}");
            assert!(
                lines
                    .iter()
                    .all(|line| UnicodeWidthStr::width(plain(line).as_str()) <= 24),
                "{name}: {rendered}"
            );
            assert!(rendered.starts_with("  └ "), "{name}: {rendered}");
        }
    }

    /// 给定有无描述的 Shell 调用；当显示活动附件时，则只保留描述和单行命令。
    #[test]
    fn formats_shell_preview() {
        let mut tool = ToolUseBlock {
            id: "shell-1".into(),
            name: "bash".into(),
            input: [("command".into(), serde_json::json!("cargo check"))]
                .into_iter()
                .collect(),
        };
        let direct = render_activity_preview(&tool, 80, None);
        assert_eq!(
            direct.iter().map(plain).collect::<Vec<_>>(),
            ["  └ $ cargo check"]
        );
        let standalone = render_tool(&tool, None, None, None, 80, None);
        assert_eq!(
            standalone.iter().map(plain).collect::<Vec<_>>(),
            ["$ cargo check"]
        );

        tool.input
            .insert("description".into(), serde_json::json!("Check workspace"));
        let described = render_activity_preview(&tool, 80, None);
        assert_eq!(
            described.iter().map(plain).collect::<Vec<_>>(),
            ["  └ Check workspace", "    $ cargo check"]
        );
        let standalone = render_tool(&tool, None, None, None, 80, None);
        assert_eq!(
            standalone.iter().map(plain).collect::<Vec<_>>(),
            ["Check workspace", "$ cargo check"]
        );
    }

    /// 给定指定目录的搜索；当显示活动附件时，则查询与搜索范围占同一行。
    #[test]
    fn combines_search_scope() {
        let tool = ToolUseBlock {
            id: "search-1".into(),
            name: "search".into(),
            input: [
                ("query".into(), serde_json::json!("AgentRun")),
                ("path".into(), serde_json::json!("src")),
            ]
            .into_iter()
            .collect(),
        };
        let lines = render_activity_preview(&tool, 80, None);
        assert_eq!(
            lines.iter().map(plain).collect::<Vec<_>>(),
            ["  └ ⌕ Search  AgentRun in src"]
        );
        let standalone = render_tool(&tool, None, None, None, 80, None);
        assert_eq!(
            standalone.iter().map(plain).collect::<Vec<_>>(),
            ["⌕ Search  AgentRun in src"]
        );
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

        assert_eq!(plain(&lines[0]), "/ Skill commit-message");
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
        assert_eq!(plain(&lines[0]), "◇ Image /tmp/image.png");
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

        assert_eq!(plain(&lines[0]), "◇ Image /tmp/image.png");
        assert_eq!(plain(&lines[1]), "  Failed to read image /tmp/image.png");
        assert_eq!(lines[1].spans[1].style.fg, Some(crate::ui::theme::ERROR));
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
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(crate::app::event::PermissionPreview::Read(
                crate::app::event::ReadPermissionPreview {
                    file_path: "/tmp/image.png".to_string(),
                },
            )),
        };

        let lines = render_tool(&tool_use, None, Some(&preview), Some(false), 80, None);

        assert_eq!(plain(&lines[0]), "⏺ View Image /tmp/image.png");
        assert_eq!(lines.len(), 1);
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

        assert_eq!(plain(&lines[0]), "创建提交");
        assert!(plain(&lines[1]).starts_with("$ git commit"));
        assert_eq!(plain(&lines[2]), "  Permission denied for tool: bash");
        assert_eq!(lines[0].spans[0].style.fg, Some(crate::ui::theme::MUTED));
        assert_eq!(lines[2].spans[0].style.fg, Some(crate::ui::theme::ERROR));
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

        assert!(rendered[0].contains("docs/search"));
        assert!(rendered.iter().any(|line| line.contains("Parameters")));
        assert!(rendered.iter().any(|line| line.contains("rust")));
        assert!(rendered.iter().any(|line| line.contains("Result")));
        assert!(rendered.iter().any(|line| line.contains("found docs")));
    }

    #[test]
    fn pending_mcp_tool_keeps_call_summary() {
        let mut input = std::collections::HashMap::new();
        input.insert("query".to_string(), serde_json::json!("rust"));
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "mcp__docs__search".to_string(),
            input,
        };

        let lines = render_tool(&tool_use, None, None, None, 80, None);

        assert!(plain(&lines[0]).contains("docs/search"));
        assert!(!plain(&lines[0]).contains("运行中"));
        assert!(lines.iter().any(|line| plain(line).contains("rust")));
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
        assert!(
            !lines
                .iter()
                .any(|line| plain(line).contains("required_action"))
        );
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

        assert_eq!(rendered[0], "⏺ Checklist");
        assert_eq!(rendered[1], "  └ ✔ Read existing flow");
        assert_eq!(rendered[2], "    ◐ Add UpdateTodo widget");
        assert_eq!(rendered[3], "    □ Run focused tests");
        assert_eq!(rendered[4], "    — Drop obsolete step");
        assert!(rendered.iter().all(|line| !line.contains("\"todos\"")));
        assert_eq!(lines[2].spans[3].style.fg, Some(crate::ui::theme::ACCENT));
        assert_eq!(lines[2].spans[1].style.fg, Some(crate::ui::theme::RUNNING));
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

pub mod agent;
pub mod fallback;
