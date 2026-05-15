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
    let available = content_width.saturating_sub(2);
    let mut lines: Vec<Line> = Vec::new();

    let mut content_style = if italic {
        Style::default()
            .fg(border_color)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default()
    };
    if let Some(c) = bg {
        content_style = content_style.bg(c);
    }

    let border_span = |c: Color| -> Span<'static> {
        let mut style = Style::default().fg(c);
        if let Some(bg_c) = bg {
            style = style.bg(bg_c);
        }
        Span::styled("\u{2503}", style)
    };

    let space_span = || -> Span<'static> {
        if let Some(c) = bg {
            Span::raw(" ").style(Style::default().bg(c))
        } else {
            Span::raw(" ")
        }
    };

    if text.is_empty() {
        lines.push(Line::from(vec![border_span(border_color)]));
    } else {
        let wrapped = word_wrap(text, available);
        for wrapped_line in wrapped {
            lines.push(Line::from(vec![
                border_span(border_color),
                space_span(),
                Span::styled(wrapped_line, content_style),
            ]));
        }
    }

    lines
}

pub fn build_plain_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let wrapped = word_wrap(text, content_width);
    wrapped.into_iter().map(Line::from).collect()
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

    if text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("\u{2503}", border_style),
            Span::raw(" "),
            Span::styled("Thinking: ", prefix_style),
        ]));
        return lines;
    }

    let prefix = "Thinking: ";
    let prefix_w = UnicodeWidthStr::width(prefix);
    let first_line_available = available.saturating_sub(prefix_w);
    let logical_lines: Vec<&str> = text.split('\n').collect();

    for (ll_idx, ll) in logical_lines.iter().enumerate() {
        let is_first = ll_idx == 0;

        if is_first && first_line_available == 0 {
            // Terminal too narrow for prefix + any content on one line
            lines.push(Line::from(vec![
                Span::styled("\u{2503}", border_style),
                Span::raw(" "),
                Span::styled(prefix, prefix_style),
            ]));
            let wrapped = word_wrap(ll, available);
            for wl in wrapped {
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(wl, text_style),
                ]));
            }
            continue;
        }

        if ll.is_empty() {
            // Empty logical line from explicit newline: show an empty line
            if is_first {
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(prefix, prefix_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                ]));
            }
            continue;
        }

        if is_first {
            // First logical line: fit content after "Thinking: "
            let first_w = UnicodeWidthStr::width(*ll);
            if prefix_w + first_w <= available {
                // Entire first line fits after prefix
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(prefix, prefix_style),
                    Span::styled(ll.to_string(), text_style),
                ]));
            } else {
                // Split: first part after prefix, rest re-wrapped at full width
                let first_wrapped = word_wrap(ll, first_line_available);
                let first_chunk = first_wrapped.first().cloned().unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(prefix, prefix_style),
                    Span::styled(first_chunk, text_style),
                ]));
                if first_wrapped.len() > 1 {
                    let rest = first_wrapped[1..].join(" ");
                    let rest_wrapped = word_wrap(&rest, available);
                    for rl in rest_wrapped {
                        lines.push(Line::from(vec![
                            Span::styled("\u{2503}", border_style),
                            Span::raw(" "),
                            Span::styled(rl, text_style),
                        ]));
                    }
                }
            }
        } else {
            // Subsequent logical lines: wrap at full width
            let wrapped = word_wrap(ll, available);
            for wl in wrapped {
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(wl, text_style),
                ]));
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
        "read" => read::render(tool_use, tool_result, content_width, project_dir),
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
