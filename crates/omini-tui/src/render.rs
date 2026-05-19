use crate::selection::{highlighted_line, selected_cols_for_screen_line};
use crate::state::{
    AgentCreateStep, AgentEditorField, AgentManagerState, AgentManagerView, AgentModelEntry,
    InteractionStep, ModelSelectionEntry, SubagentNode, UiMessage, UiState,
};
use crate::types::display::{DisplayMention, DisplayMessage, MentionKind};
use crate::types::events::{PermissionPreview, SubagentStatus, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ContentBlock, TextBlock, ToolResultBlock, ToolUseBlock};
use crate::widgets::{
    build_bordered_lines, build_plain_lines, build_thinking_lines, display_path, render_tool,
};
use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod agents;
mod autocomplete;
mod input;
mod interactions;
mod layout;
mod permission_drawer;
mod status;

const PERMISSION_DRAWER_MAX_HEIGHT: u16 = 18;
const EDIT_PERMISSION_DRAWER_MAX_HEIGHT: u16 = 50;
const AGENT_EDITOR_MAX_WIDTH: usize = 140;
const AGENT_TOOLS_SECTION_LINES: usize = 21;
const AGENT_EDIT_CONTENT_INSTRUCTIONS_MAX_LINES: usize = 10;
const USER_INPUT_NONE_LABEL: &str = "以上都不是";
const USER_INPUT_NONE_DESCRIPTION: &str = "可按 Tab 在备注中补充说明。";
const USER_INPUT_NOTE_PREFIX: &str = "› ";
const USER_INPUT_NOTE_PLACEHOLDER: &str = "添加备注";

pub fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    layout::render(state, frame);
}

pub(super) fn render_session_list(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    let Some(InteractionStep::Session {
        sessions,
        selected,
        search,
        ..
    }) = state.interaction_step.clone()
    else {
        return;
    };

    let total = sessions.len();

    // Layout: header(1) + content(fill) + divider(1) + footer(1)
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let header_area = chunks[0];
    let content_area = chunks[1];
    let divider_area = chunks[2];
    let footer_area = chunks[3];

    let content_w = content_area.width as usize;
    let content_h = content_area.height as usize;

    // ── Header ──
    let header_style = Style::default()
        .fg(Color::Rgb(0xa5, 0xac, 0xb6))
        .add_modifier(Modifier::BOLD);
    let filter_style = Style::default().fg(Color::Rgb(0x6f, 0x76, 0x83));
    let mut header_lines: Vec<Line> = vec![
        Line::from(Span::styled("会话", header_style)),
        if search.is_empty() {
            Line::from(Span::styled("直接输入关键词筛选会话", filter_style))
        } else {
            Line::from(Span::styled(format!("筛选：{}", search), filter_style))
        },
    ];
    register_and_highlight_lines(state, header_area, &mut header_lines);
    frame.render_widget(Paragraph::new(header_lines), header_area);

    // ── Content ──
    let mut lines: Vec<Line> = Vec::with_capacity(content_h);
    let mut row_backgrounds: Vec<Option<Color>> = Vec::with_capacity(content_h);

    if total == 0 {
        // Empty state
        lines.push(Line::from(Span::styled(
            pad_display_width("没有找到会话", content_w),
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        )));
        row_backgrounds.push(None);
        while lines.len() < content_h {
            lines.push(Line::from(Span::styled(
                " ".repeat(content_w),
                Style::default(),
            )));
            row_backgrounds.push(None);
        }
        register_and_highlight_lines(state, content_area, &mut lines);
        render_session_row_backgrounds(frame, content_area, &row_backgrounds);
        frame.render_widget(Paragraph::new(lines), content_area);

        // Divider (empty)
        let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
        let mut divider_line = Line::from(Span::styled("─".repeat(content_w), divider_style));
        register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
        frame.render_widget(Paragraph::new(divider_line), divider_area);

        // Footer
        let mut footer_line = Line::from(Span::styled(
            "Esc 返回 · 输入筛选",
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        ));
        register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer_line));
        frame.render_widget(Paragraph::new(footer_line), footer_area);
        return;
    }

    // Scroll calculation
    let item_lines = content_h.saturating_sub(2); // reserve top/bottom for indicators
    let mut scroll_off = 0usize;
    if total > item_lines {
        // Keep selected item visible, prefer centering
        let ideal = selected.saturating_sub(item_lines / 2);
        scroll_off = ideal.min(total.saturating_sub(item_lines));
    }
    let show_top = scroll_off > 0;
    let show_bot = scroll_off + item_lines < total;

    let max_visible = item_lines.min(total.saturating_sub(scroll_off));
    let time_col_w = 8; // "59分钟前" / "23小时前" are both 8 display cells.
    let prefix_w = UnicodeWidthStr::width("❯ ");
    let separator_w = UnicodeWidthStr::width("  ");
    let max_msg_w = content_w.saturating_sub(prefix_w + time_col_w + separator_w);

    // ── Build lines ──
    // Top indicator
    if show_top {
        lines.push(Line::from(Span::styled(
            pad_display_width("↑ 更多", content_w),
            Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
        )));
        row_backgrounds.push(None);
    } else {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
        row_backgrounds.push(None);
    }

    // Session items
    for i in 0..max_visible {
        let actual_idx = scroll_off + i;
        let session = &sessions[actual_idx];
        let is_selected = actual_idx == selected;

        let bg = if is_selected {
            Some(Color::Rgb(0x41, 0x45, 0x4c))
        } else if actual_idx.is_multiple_of(2) {
            Some(Color::Rgb(0x33, 0x37, 0x3f))
        } else {
            None
        };

        let fg = if is_selected {
            Color::Rgb(0xc1, 0x97, 0x72)
        } else {
            Color::Rgb(0xa5, 0xac, 0xb6)
        };

        let prefix = if is_selected { "❯ " } else { "  " };
        let time_str = pad_display_width(&relative_time(session.created_at), time_col_w);
        let msg = truncate_str(&session.title, max_msg_w);
        let line_content = format!("{}{}  {}", prefix, time_str, msg);
        let padded = pad_display_width(&line_content, content_w);

        lines.push(Line::from(Span::styled(
            padded,
            match bg {
                Some(bg) => Style::default().fg(fg).bg(bg),
                None => Style::default().fg(fg),
            },
        )));
        row_backgrounds.push(bg);
    }

    // Fill remaining item lines
    while lines.len() < content_h - 1 {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
        row_backgrounds.push(None);
    }

    // Bottom indicator
    if show_bot {
        lines.push(Line::from(Span::styled(
            pad_display_width("↓ 更多", content_w),
            Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
        )));
        row_backgrounds.push(None);
    } else {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
        row_backgrounds.push(None);
    }

    register_and_highlight_lines(state, content_area, &mut lines);
    render_session_row_backgrounds(frame, content_area, &row_backgrounds);
    frame.render_widget(Paragraph::new(lines), content_area);

    // ── Divider ──
    let current = selected + 1;
    let indicator = format!(" {}/{} ", current, total);
    let dashes_count = content_w.saturating_sub(indicator.len());
    let divider_line = format!("{}{}", "─".repeat(dashes_count), indicator);
    let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
    let mut divider_line = Line::from(Span::styled(divider_line, divider_style));
    register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
    frame.render_widget(Paragraph::new(divider_line), divider_area);

    // ── Footer ──
    let mut footer_line = Line::from(Span::styled(
        "↑/↓ 选择 · Enter 确认 · Esc 返回 · 输入筛选",
        Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
    ));
    register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer_line));
    frame.render_widget(Paragraph::new(footer_line), footer_area);
}

fn render_session_row_backgrounds(
    frame: &mut ratatui::Frame,
    area: Rect,
    row_backgrounds: &[Option<Color>],
) {
    let row_fill = " ".repeat(area.width as usize);
    for (idx, bg) in row_backgrounds.iter().enumerate() {
        let Some(bg) = bg else {
            continue;
        };
        if idx >= area.height as usize {
            break;
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                row_fill.clone(),
                Style::default().bg(*bg),
            ))),
            Rect {
                x: area.x,
                y: area.y + idx as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// 将 UTC 时间格式化为相对时间（如 "3分钟前", "2小时前", "5天前"）。
fn relative_time(utc: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(utc);
    let seconds = duration.num_seconds().max(0);
    if seconds < 60 {
        "刚刚".to_string()
    } else if seconds < 3600 {
        format!("{}分钟前", seconds / 60)
    } else if seconds < 86400 {
        format!("{}小时前", seconds / 3600)
    } else if seconds < 604800 {
        format!("{}天前", seconds / 86400)
    } else if seconds < 2592000 {
        format!("{}周前", seconds / 604800)
    } else {
        // 超过一个月显示日期
        utc.with_timezone(&Local).format("%m-%d").to_string()
    }
}

/// 截断字符串到指定显示宽度，超长时末尾补 "..."。
fn truncate_str(s: &str, max_width: usize) -> String {
    if max_width == 0 || s.is_empty() {
        return String::new();
    }
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }
    let ellipsis = "...";
    let target = max_width.saturating_sub(UnicodeWidthStr::width(ellipsis));
    let mut result = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > target {
            break;
        }
        result.push(c);
        w += cw;
    }
    result.push_str(ellipsis);
    result
}

fn pad_display_width(s: &str, width: usize) -> String {
    let current = UnicodeWidthStr::width(s);
    if current >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - current))
    }
}

fn styled_wrapped_text(
    text_block: &TextBlock,
    content_width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    styled_wrapped_ranges(&text_block.text, &[], content_width, base_style)
}

fn styled_wrapped_display(
    display: &DisplayMessage,
    content_width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    styled_wrapped_ranges(&display.text, &display.mentions, content_width, base_style)
}

fn styled_wrapped_ranges(
    text: &str,
    mentions: &[DisplayMention],
    content_width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }

    let mention_ranges = mentions
        .iter()
        .map(|mention| (mention.start_char, mention.end_char, mention.kind))
        .collect::<Vec<_>>();
    let image_ranges = image_marker_ranges(text);
    let mention_style = base_style
        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
        .add_modifier(Modifier::BOLD);
    let normal_style = base_style;
    let width_limit = content_width.max(1);
    let mut lines = Vec::new();

    for logical in split_with_char_offsets(text) {
        let mut spans = Vec::new();
        let mut current_width = 0usize;
        for (char_idx, ch) in logical {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width + ch_width > width_limit {
                lines.push(Line::from(spans));
                spans = Vec::new();
                current_width = 0;
            }
            let style = if let Some((_, _, kind)) = mention_ranges
                .iter()
                .find(|(start, end, _)| char_idx >= *start && char_idx < *end)
            {
                match kind {
                    MentionKind::Subagent
                    | MentionKind::File
                    | MentionKind::Directory
                    | MentionKind::Command => mention_style,
                }
            } else if image_ranges
                .iter()
                .any(|(start, end)| char_idx >= *start && char_idx < *end)
            {
                mention_style
            } else {
                normal_style
            };
            push_char_span(&mut spans, ch, style);
            current_width += ch_width;
        }
        lines.push(Line::from(spans));
    }

    lines
}

fn image_marker_ranges(text: &str) -> Vec<(usize, usize)> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut idx = 0usize;
    while idx < chars.len() {
        if chars[idx] != '[' {
            idx += 1;
            continue;
        }
        let prefix = ['[', 'I', 'm', 'a', 'g', 'e', '#'];
        if idx + prefix.len() >= chars.len() || chars[idx..idx + prefix.len()] != prefix {
            idx += 1;
            continue;
        }
        let mut end = idx + prefix.len();
        let digit_start = end;
        while end < chars.len() && chars[end].is_ascii_digit() {
            end += 1;
        }
        if end > digit_start && chars.get(end) == Some(&']') {
            ranges.push((idx, end + 1));
            idx = end + 1;
        } else {
            idx += 1;
        }
    }
    ranges
}

fn split_with_char_offsets(text: &str) -> Vec<Vec<(usize, char)>> {
    let mut lines: Vec<Vec<(usize, char)>> = vec![Vec::new()];
    for (idx, ch) in text.chars().enumerate() {
        if ch == '\n' {
            lines.push(Vec::new());
        } else if let Some(line) = lines.last_mut() {
            line.push((idx, ch));
        }
    }
    lines
}

fn push_char_span(spans: &mut Vec<Span<'static>>, ch: char, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(ch);
        return;
    }
    spans.push(Span::styled(ch.to_string(), style));
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn build_display_message_lines(
    display: &DisplayMessage,
    content_width: usize,
) -> Vec<Line<'static>> {
    let user_bg = Color::Rgb(65, 69, 76);
    let bg_style = Style::default().bg(user_bg);
    let mut lines =
        vec![Line::from(Span::styled(" ".repeat(content_width), bg_style)).style(bg_style)];

    let wrapped = styled_wrapped_display(display, content_width.saturating_sub(2), bg_style);
    if wrapped.is_empty() {
        let text = format!("❯ {}", " ".repeat(content_width.saturating_sub(2)));
        lines.push(Line::from(Span::styled(text, bg_style)).style(bg_style));
    } else {
        for (idx, wl) in wrapped.into_iter().enumerate() {
            let prefix = if idx == 0 { "❯ " } else { "  " };
            let text_width = UnicodeWidthStr::width(prefix) + line_width(&wl);
            let remaining = content_width.saturating_sub(text_width);
            let mut spans = vec![Span::styled(prefix, bg_style)];
            spans.extend(wl.spans);
            spans.push(Span::styled(" ".repeat(remaining), bg_style));
            lines.push(Line::from(spans).style(bg_style));
        }
    }

    lines.push(Line::from(Span::styled(" ".repeat(content_width), bg_style)).style(bg_style));
    lines
}

// ===========================================================================
// Messages, Input, Footer (原逻辑不变)
// ===========================================================================

fn render_subagent_tool(
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

enum AlertKind {
    Notice,
    Warning,
    Error,
}

fn build_alert_lines(text: &str, content_width: usize, kind: AlertKind) -> Vec<Line<'static>> {
    let (icon, color) = match kind {
        AlertKind::Notice => ("ℹ", Color::Rgb(0x7a, 0xba, 0xff)),
        AlertKind::Warning => ("⚠", Color::Rgb(0xd4, 0xb6, 0x6a)),
        AlertKind::Error => ("✖", Color::Rgb(255, 100, 100)),
    };
    let prefix = format!("{icon} ");
    let wrap_width = content_width
        .saturating_sub(UnicodeWidthStr::width(prefix.as_str()))
        .max(1);
    let wrapped = crate::widgets::word_wrap(text, wrap_width);
    let style = Style::default().fg(color);
    if wrapped.is_empty() {
        return vec![Line::from(vec![Span::styled(prefix, style)])];
    }

    let continuation = " ".repeat(UnicodeWidthStr::width(prefix.as_str()));
    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let prefix = if idx == 0 {
                prefix.clone()
            } else {
                continuation.clone()
            };
            Line::from(vec![Span::styled(format!("{prefix}{line}"), style)])
        })
        .collect()
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

pub(super) fn render_messages(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if state.messages.is_empty() && state.pending_assistant.is_none() {
        state.selectable_message_lines.clear();
        state.message_scroll_y = 0;
        return;
    }

    let content_width = area.width as usize;
    let visible_height = area.height as usize;
    let mut all_lines: Vec<Line> = Vec::new();
    let mut selectable_lines: Vec<String> = Vec::new();

    let rendered_messages: Vec<&crate::types::message::Message> = state
        .messages
        .iter()
        .filter_map(UiMessage::as_message)
        .collect();

    let mut tool_result_map: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (mi, msg) in rendered_messages.iter().enumerate() {
        for (bi, block) in msg.content.iter().enumerate() {
            if let ContentBlock::ToolResult(tr) = block {
                tool_result_map
                    .entry(tr.tool_use_id.clone())
                    .or_default()
                    .push((mi, bi));
            }
        }
    }
    let mut consumed: HashSet<(usize, usize)> = HashSet::new();

    let mut rendered_msg_idx = 0;
    for ui_message in &state.messages {
        if let UiMessage::Display(display) = ui_message {
            let block_lines = build_display_message_lines(display, content_width);
            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.extend(block_lines);
            }
            continue;
        }

        let UiMessage::Message(message) = ui_message else {
            let block_lines = match ui_message {
                UiMessage::Notice { text } => {
                    build_alert_lines(text, content_width, AlertKind::Notice)
                }
                UiMessage::Warning { text } => {
                    build_alert_lines(text, content_width, AlertKind::Warning)
                }
                UiMessage::Error { text } => {
                    build_alert_lines(text, content_width, AlertKind::Error)
                }
                UiMessage::Display(_) => unreachable!(),
                UiMessage::Message(_) => unreachable!(),
            };
            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.extend(block_lines);
            }
            continue;
        };

        let msg_idx = rendered_msg_idx;
        rendered_msg_idx += 1;

        for (block_idx, block) in message.content.iter().enumerate() {
            if let ContentBlock::ToolResult(_) = block
                && consumed.contains(&(msg_idx, block_idx))
            {
                continue;
            }

            let mut block_lines: Vec<Line> = Vec::new();
            match block {
                ContentBlock::Text(tb) if message.role == crate::types::message::Role::User => {
                    let user_bg = Color::Rgb(65, 69, 76);
                    let bg_style = Style::default().bg(user_bg);
                    block_lines.push(
                        Line::from(Span::styled(" ".repeat(content_width), bg_style))
                            .style(bg_style),
                    );

                    let wrapped = styled_wrapped_text(
                        tb,
                        content_width.saturating_sub(2),
                        Style::default().bg(user_bg),
                    );
                    if wrapped.is_empty() {
                        let text = format!("❯ {}", " ".repeat(content_width.saturating_sub(2)));
                        block_lines.push(Line::from(Span::styled(text, bg_style)).style(bg_style));
                    } else {
                        for (idx, wl) in wrapped.into_iter().enumerate() {
                            let prefix = if idx == 0 { "❯ " } else { "  " };
                            let text_width = UnicodeWidthStr::width(prefix) + line_width(&wl);
                            let remaining = content_width.saturating_sub(text_width);
                            let mut spans = vec![Span::styled(prefix, bg_style)];
                            spans.extend(wl.spans);
                            spans.push(Span::styled(" ".repeat(remaining), bg_style));
                            block_lines.push(Line::from(spans).style(bg_style));
                        }
                    }

                    block_lines.push(
                        Line::from(Span::styled(" ".repeat(content_width), bg_style))
                            .style(bg_style),
                    );
                }
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::Image(_) => {}
                ContentBlock::ToolUse(tu) => {
                    if tu.name == "subagent" {
                        let node = state
                            .subagents_by_tool_use
                            .get(&tu.id)
                            .and_then(|session_id| state.subagents.get(session_id));
                        let tool_result = tool_result_map.get(&tu.id).and_then(|positions| {
                            positions.first().and_then(|(mi, bi)| {
                                if let ContentBlock::ToolResult(tr) =
                                    &rendered_messages[*mi].content[*bi]
                                {
                                    Some(tr.clone())
                                } else {
                                    None
                                }
                            })
                        });
                        block_lines.extend(render_subagent_tool(
                            tu,
                            tool_result.as_ref(),
                            node,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        ));
                        if let Some(positions) = tool_result_map.get(&tu.id) {
                            for pos in positions {
                                consumed.insert(*pos);
                            }
                        }
                    } else if let Some(positions) = tool_result_map.get(&tu.id) {
                        let tool_result = positions.first().and_then(|(mi, bi)| {
                            if let ContentBlock::ToolResult(tr) =
                                &rendered_messages[*mi].content[*bi]
                            {
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });

                        let tool_lines = render_tool(
                            tu,
                            tool_result.as_ref(),
                            None,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        );
                        block_lines.extend(tool_lines);

                        for pos in positions {
                            consumed.insert(*pos);
                        }
                    } else {
                        // 工具结果尚未返回
                        let tool_lines = render_tool(
                            tu,
                            None,
                            None,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        );
                        block_lines.extend(tool_lines);
                    }
                }
                ContentBlock::Thinking(tb) => {
                    let mut lines = build_thinking_lines(&tb.thinking, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolResult(tr) => {
                    let color = if tr.is_error {
                        Color::Rgb(255, 100, 100)
                    } else {
                        Color::Rgb(100, 200, 130)
                    };
                    let mut lines =
                        build_bordered_lines(&tr.content, content_width, color, false, None);
                    block_lines.append(&mut lines);
                }
            }

            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.append(&mut block_lines);
            }
        }
    }

    // ===== 渲染 pending_assistant（流式增量内容） =====
    if let Some(pending) = &state.pending_assistant {
        // 先构建 pending_assistant 内部的 tool_result_map
        let mut tr_indices: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for (bi, block) in pending.content.iter().enumerate() {
            if let ContentBlock::ToolResult(tr) = block {
                tr_indices.entry(tr.tool_use_id.clone()).or_insert(bi);
            }
        }
        let mut consumed_tr: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for (block_idx, block) in pending.content.iter().enumerate() {
            if let ContentBlock::ToolResult(_) = block
                && consumed_tr.contains(&block_idx)
            {
                continue;
            }

            let mut block_lines: Vec<Line> = Vec::new();
            match block {
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::Image(_) => {}
                ContentBlock::Thinking(tb) => {
                    let mut lines = build_thinking_lines(&tb.thinking, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    if tu.name == "subagent" {
                        let node = state
                            .subagents_by_tool_use
                            .get(&tu.id)
                            .and_then(|session_id| state.subagents.get(session_id));
                        let tr = tr_indices.get(&tu.id).and_then(|&bi| {
                            if let ContentBlock::ToolResult(tr) = &pending.content[bi] {
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });
                        block_lines.extend(render_subagent_tool(
                            tu,
                            tr.as_ref(),
                            node,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        ));
                        if let Some(&bi) = tr_indices.get(&tu.id) {
                            consumed_tr.insert(bi);
                        }
                    } else {
                        // 检查是否有对应的 ToolResult
                        let tr = tr_indices.get(&tu.id).and_then(|&bi| {
                            if let ContentBlock::ToolResult(tr) = &pending.content[bi] {
                                consumed_tr.insert(bi);
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });
                        let tool_lines = render_tool(
                            tu,
                            tr.as_ref(),
                            None,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        );
                        block_lines.extend(tool_lines);
                    }
                }
                ContentBlock::ToolResult(tr) => {
                    // 如果没有对应的 ToolUse 来消费它，单独渲染
                    let color = if tr.is_error {
                        Color::Rgb(255, 100, 100)
                    } else {
                        Color::Rgb(100, 200, 130)
                    };
                    let mut lines =
                        build_bordered_lines(&tr.content, content_width, color, false, None);
                    block_lines.append(&mut lines);
                }
            }

            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.append(&mut block_lines);
            }
        }
    }

    let total_lines = all_lines.len();
    let prev_total_lines = state.total_lines;
    state.total_lines = total_lines;
    if total_lines == 0 {
        return;
    }

    if !state.auto_scroll {
        let delta = total_lines.saturating_sub(prev_total_lines);
        state.scroll_offset = state.scroll_offset.saturating_add(delta);
    }

    let max_scroll = total_lines.saturating_sub(visible_height);
    let capped_offset = state.scroll_offset.min(max_scroll);
    state.scroll_offset = capped_offset;
    let scroll_y = max_scroll.saturating_sub(capped_offset);
    state.selectable_message_lines = selectable_lines;
    state.message_scroll_y = scroll_y;
    let visible_selectable_lines = state
        .selectable_message_lines
        .iter()
        .skip(scroll_y)
        .take(visible_height)
        .cloned()
        .collect::<Vec<_>>();
    for (visible_row, text) in visible_selectable_lines.into_iter().enumerate() {
        state.register_selectable_screen_line(
            area.y + visible_row as u16,
            area.x,
            area.width,
            text,
        );
    }

    let user_bg = Color::Rgb(65, 69, 76);
    let user_line_bg = Style::default().bg(user_bg);
    let user_line_rows = all_lines
        .iter()
        .map(|line| line.style.bg == Some(user_bg))
        .collect::<Vec<_>>();

    apply_text_selection_highlight(state, &mut all_lines, area, scroll_y, visible_height);

    for (idx, is_user_line) in user_line_rows.iter().copied().enumerate().skip(scroll_y) {
        if !is_user_line {
            continue;
        }
        let visible_row = idx - scroll_y;
        if visible_row >= visible_height {
            break;
        }
        frame.buffer_mut().set_style(
            Rect::new(area.x, area.y + visible_row as u16, area.width, 1),
            user_line_bg,
        );
    }

    let paragraph =
        Paragraph::new(ratatui::text::Text::from(all_lines)).scroll((scroll_y as u16, 0));

    frame.render_widget(paragraph, area);
}

fn line_to_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn register_and_highlight_lines(state: &mut UiState, area: Rect, lines: &mut [Line<'static>]) {
    for (idx, line) in lines.iter().enumerate() {
        state.register_selectable_screen_line(
            area.y + idx as u16,
            area.x,
            area.width,
            line_to_plain_text(line),
        );
    }

    let highlight = Style::default()
        .fg(Color::Rgb(40, 44, 52))
        .bg(Color::Rgb(180, 210, 255))
        .add_modifier(Modifier::BOLD);

    for (idx, line) in lines.iter_mut().enumerate() {
        let text = line_to_plain_text(line);
        let screen_row = area.y + idx as u16;
        if let Some((start_col, end_col)) = selected_cols_for_screen_line(state, screen_row, &text)
        {
            *line = highlighted_line(&text, start_col, end_col, highlight);
        }
    }
}

fn apply_text_selection_highlight(
    state: &UiState,
    lines: &mut [Line<'static>],
    area: Rect,
    scroll_y: usize,
    visible_height: usize,
) {
    let highlight = Style::default()
        .fg(Color::Rgb(40, 44, 52))
        .bg(Color::Rgb(180, 210, 255))
        .add_modifier(Modifier::BOLD);

    for content_row in scroll_y
        ..state
            .selectable_message_lines
            .len()
            .min(scroll_y.saturating_add(visible_height))
    {
        let Some(text) = state.selectable_message_lines.get(content_row) else {
            continue;
        };
        let screen_row = area.y + (content_row - scroll_y) as u16;
        if let (Some((start_col, end_col)), Some(line)) = (
            selected_cols_for_screen_line(state, screen_row, text),
            lines.get_mut(content_row),
        ) {
            *line = highlighted_line(text, start_col, end_col, highlight);
        }
    }
}
