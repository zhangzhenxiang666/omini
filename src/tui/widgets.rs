use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

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
    collapsed: bool,
) -> Vec<Line<'static>> {
    match tool_use.name.as_str() {
        "bash" => render_bash(tool_use, tool_result, content_width, collapsed),
        "read" => render_read(tool_use, tool_result, content_width),
        "edit" => render_edit(tool_use, tool_result, tool_preview, content_width),
        "write" => render_write(tool_use, tool_result, tool_preview, content_width),
        _ => Vec::new(),
    }
}

fn render_read(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let file_path = tool_use
        .input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");

    // Running state: spinner before "-> Read <path>"
    let read_color = Color::Rgb(38, 42, 50);
    let mut main_spans = Vec::new();

    if result.is_none() {
        let spin = spinner();
        main_spans.push(Span::styled(
            format!("{} ", spin),
            Style::default().fg(Color::Rgb(212, 182, 106)),
        ));
    }

    main_spans.push(Span::raw("· "));
    main_spans.push(Span::styled("Read", Style::default().fg(read_color)));
    main_spans.push(Span::raw(format!(" {}", file_path)));

    let main_line = Line::from(main_spans);
    lines.push(main_line);

    // If there's an error, show it in red below
    if let Some(tr) = result
        && tr.is_error
    {
        let error_style = Style::default().fg(Color::Rgb(255, 100, 100));
        let wrapped = word_wrap(&tr.content, content_width.saturating_sub(2));
        for wl in wrapped {
            lines.push(Line::from(vec![Span::styled(wl, error_style)]));
        }
    }

    lines
}

fn render_edit(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let file_path = tool_use
        .input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let old_string = tool_use
        .input
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_string = tool_use
        .input
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let replace_all = tool_use
        .input
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let green_fg = Color::Rgb(0x50, 0xc8, 0x78);
    let red_fg = Color::Rgb(255, 100, 100);
    let add_bg = Color::Rgb(35, 55, 40);
    let del_bg = Color::Rgb(55, 35, 38);
    let ctx_bg = Color::Rgb(40, 44, 52);
    let header_bg = Color::Rgb(38, 42, 50);
    let w = content_width;

    let make_line_bg = |text: &str, fg: Color, bg: Color| -> Line<'static> {
        let text_w = UnicodeWidthStr::width(text);
        let pad = w.saturating_sub(text_w);
        let style = Style::default().fg(fg).bg(bg);
        let mut spans = vec![Span::styled(text.to_string(), style)];
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
        Line::from(spans)
    };

    if let Some(tr) = result
        && tr.is_error
    {
        lines.push(Line::from(vec![
            Span::raw("· "),
            Span::styled("Edit", Style::default().fg(accent)),
        ]));
        let error_style = Style::default().fg(red_fg);
        let wrapped = word_wrap(&tr.content, w.saturating_sub(2));
        for wl in wrapped {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wl, error_style),
            ]));
        }
        return lines;
    }

    let preview_start_lines: Vec<usize> = preview
        .and_then(|req| match &req.kind {
            ToolPauseKind::Permission(PermissionPreview::Edit(preview)) => {
                Some(preview.start_lines.clone())
            }
            _ => None,
        })
        .unwrap_or_default();

    let replacement_count = result
        .and_then(|tr| tr.metadata.as_ref())
        .and_then(|m| m.get("replacement_count"))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or_else(|| {
            if !preview_start_lines.is_empty() {
                preview_start_lines.len()
            } else if replace_all {
                0
            } else {
                1
            }
        });
    let (added_per_match, removed_per_match) = count_edit_line_changes(old_string, new_string);
    let total_added = added_per_match * replacement_count.max(1);
    let total_removed = removed_per_match * replacement_count.max(1);

    let mut header_spans: Vec<Span<'static>> = Vec::new();
    let is_running_without_preview = result.is_none() && preview.is_none();
    if is_running_without_preview {
        header_spans.push(Span::styled(
            format!("{} ", spinner()),
            Style::default().fg(Color::Rgb(212, 182, 106)),
        ));
        header_spans.push(Span::styled("Edit", Style::default().fg(accent)));
        lines.push(Line::from(header_spans));
        return lines;
    } else {
        let hdr_plain = Style::default().bg(header_bg);
        let hdr_accent = Style::default().fg(accent).bg(header_bg);
        let hdr_green = Style::default().fg(green_fg).bg(header_bg);
        let hdr_red = Style::default().fg(red_fg).bg(header_bg);
        header_spans.push(Span::styled("· ", hdr_plain));
        header_spans.push(Span::styled("Edit", hdr_accent));
        header_spans.push(Span::styled(format!(" {} (", file_path), hdr_plain));
        header_spans.push(Span::styled(format!("+{}", total_added), hdr_green));
        header_spans.push(Span::styled(" ", hdr_plain));
        header_spans.push(Span::styled(format!("-{}", total_removed), hdr_red));
        header_spans.push(Span::styled(")", hdr_plain));
        let total_w: usize = header_spans.iter().map(|s| s.width()).sum();
        if w > total_w {
            header_spans.push(Span::styled(
                " ".repeat(w - total_w),
                Style::default().bg(header_bg),
            ));
        }
    }
    lines.push(Line::from(header_spans));

    let result_start_lines: Vec<usize> = result
        .and_then(|tr| tr.metadata.as_ref())
        .and_then(|m| m.get("matches"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    m.get("start_line")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                })
                .collect()
        })
        .unwrap_or_default();

    let start_lines = if !result_start_lines.is_empty() {
        result_start_lines
    } else if !preview_start_lines.is_empty() {
        preview_start_lines
    } else {
        Vec::new()
    };

    let hunk_count = if start_lines.is_empty() {
        1
    } else {
        start_lines.len()
    };
    let max_line = start_lines
        .iter()
        .copied()
        .max()
        .unwrap_or(1)
        .saturating_add(old_string.lines().count().max(new_string.lines().count()));
    let line_width = max_line.to_string().len().max(3);

    for hunk_idx in 0..hunk_count {
        if hunk_idx > 0 {
            lines.push(make_line_bg("", Color::Reset, ctx_bg));
            let separator = format!("{:width$} ⋮", "", width = line_width);
            lines.push(make_line_bg(
                &format!("  {}", separator),
                Color::Reset,
                ctx_bg,
            ));
            lines.push(make_line_bg("", Color::Reset, ctx_bg));
        }

        let start_line = start_lines.get(hunk_idx).copied();
        let old_lines: Vec<&str> = old_string.lines().collect();
        let new_lines: Vec<&str> = new_string.lines().collect();
        let mut old_idx = 0;
        let mut new_idx = 0;
        let mut old_line_no = start_line.unwrap_or(1);

        while old_idx < old_lines.len() || new_idx < new_lines.len() {
            if old_idx < old_lines.len()
                && new_idx < new_lines.len()
                && old_lines[old_idx] == new_lines[new_idx]
            {
                let formatted = if start_line.is_some() {
                    format!(
                        "{:<width$}     {}",
                        old_line_no,
                        old_lines[old_idx],
                        width = line_width
                    )
                } else {
                    format!(
                        "{:>width$}     {}",
                        "",
                        old_lines[old_idx],
                        width = line_width
                    )
                };
                lines.push(make_line_bg(
                    &format!("  {}", formatted),
                    Color::Reset,
                    ctx_bg,
                ));
                old_idx += 1;
                new_idx += 1;
                old_line_no += 1;
            } else if old_idx < old_lines.len()
                && (new_idx >= new_lines.len()
                    || !new_lines[new_idx..].contains(&old_lines[old_idx]))
            {
                if new_idx < new_lines.len() && !old_lines[old_idx..].contains(&new_lines[new_idx])
                {
                    let del = if start_line.is_some() {
                        format!(
                            "{:<width$} -   {}",
                            old_line_no,
                            old_lines[old_idx],
                            width = line_width
                        )
                    } else {
                        format!(
                            "{:>width$} -   {}",
                            "",
                            old_lines[old_idx],
                            width = line_width
                        )
                    };
                    lines.push(make_line_bg(&format!("  {}", del), red_fg, del_bg));

                    let add = if start_line.is_some() {
                        format!(
                            "{:<width$} +   {}",
                            old_line_no,
                            new_lines[new_idx],
                            width = line_width
                        )
                    } else {
                        format!(
                            "{:>width$} +   {}",
                            "",
                            new_lines[new_idx],
                            width = line_width
                        )
                    };
                    lines.push(make_line_bg(&format!("  {}", add), green_fg, add_bg));

                    old_idx += 1;
                    new_idx += 1;
                    old_line_no += 1;
                } else {
                    let del = if start_line.is_some() {
                        format!(
                            "{:<width$} -   {}",
                            old_line_no,
                            old_lines[old_idx],
                            width = line_width
                        )
                    } else {
                        format!(
                            "{:>width$} -   {}",
                            "",
                            old_lines[old_idx],
                            width = line_width
                        )
                    };
                    lines.push(make_line_bg(&format!("  {}", del), red_fg, del_bg));
                    old_idx += 1;
                    old_line_no += 1;
                }
            } else if new_idx < new_lines.len() {
                let add = format!(
                    "{:>width$} +   {}",
                    "",
                    new_lines[new_idx],
                    width = line_width
                );
                lines.push(make_line_bg(&format!("  {}", add), green_fg, add_bg));
                new_idx += 1;
            }
        }
    }

    lines
}

fn count_edit_line_changes(old_string: &str, new_string: &str) -> (usize, usize) {
    let old_lines: Vec<&str> = old_string.lines().collect();
    let new_lines: Vec<&str> = new_string.lines().collect();
    let mut old_idx = 0;
    let mut new_idx = 0;
    let mut added = 0;
    let mut removed = 0;

    while old_idx < old_lines.len() || new_idx < new_lines.len() {
        if old_idx < old_lines.len()
            && new_idx < new_lines.len()
            && old_lines[old_idx] == new_lines[new_idx]
        {
            old_idx += 1;
            new_idx += 1;
        } else if old_idx < old_lines.len()
            && (new_idx >= new_lines.len() || !new_lines[new_idx..].contains(&old_lines[old_idx]))
        {
            if new_idx < new_lines.len() && !old_lines[old_idx..].contains(&new_lines[new_idx]) {
                added += 1;
                removed += 1;
                old_idx += 1;
                new_idx += 1;
            } else {
                removed += 1;
                old_idx += 1;
            }
        } else if new_idx < new_lines.len() {
            added += 1;
            new_idx += 1;
        }
    }

    (added, removed)
}

fn render_write(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let file_path = tool_use
        .input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let content = tool_use
        .input
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let green_fg = Color::Rgb(0x50, 0xc8, 0x78);
    let red_fg = Color::Rgb(255, 100, 100);
    let add_bg = Color::Rgb(35, 55, 40);
    let header_bg = Color::Rgb(38, 42, 50);
    let w = content_width;

    let make_line_bg = |text: &str, fg: Color, bg: Color| -> Line<'static> {
        let text_w = UnicodeWidthStr::width(text);
        let pad = w.saturating_sub(text_w);
        let style = Style::default().fg(fg).bg(bg);
        let mut spans = vec![Span::styled(text.to_string(), style)];
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
        Line::from(spans)
    };

    if let Some(tr) = result
        && tr.is_error
    {
        lines.push(Line::from(vec![
            Span::raw("· "),
            Span::styled("Write", Style::default().fg(accent)),
        ]));
        let error_style = Style::default().fg(red_fg);
        let wrapped = word_wrap(&tr.content, w.saturating_sub(2));
        for wl in wrapped {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wl, error_style),
            ]));
        }
        return lines;
    }

    let is_running_without_preview = result.is_none() && preview.is_none();
    if is_running_without_preview {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", spinner()),
                Style::default().fg(Color::Rgb(212, 182, 106)),
            ),
            Span::styled("Write", Style::default().fg(accent)),
        ]));
        return lines;
    }

    let added_lines = result
        .and_then(|tr| tr.metadata.as_ref())
        .and_then(|m| m.get("added_lines"))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .or_else(|| {
            preview.and_then(|req| match &req.kind {
                ToolPauseKind::Permission(PermissionPreview::Write(preview)) => {
                    Some(preview.added_lines)
                }
                _ => None,
            })
        })
        .unwrap_or_else(|| count_write_lines(content));

    let hdr_plain = Style::default().bg(header_bg);
    let hdr_accent = Style::default().fg(accent).bg(header_bg);
    let hdr_green = Style::default().fg(green_fg).bg(header_bg);
    let mut header_spans: Vec<Span<'static>> = vec![
        Span::styled("· ", hdr_plain),
        Span::styled("Write", hdr_accent),
        Span::styled(format!(" {} (", file_path), hdr_plain),
        Span::styled(format!("+{}", added_lines), hdr_green),
        Span::styled(" -0)", hdr_plain),
    ];
    let total_w: usize = header_spans.iter().map(|s| s.width()).sum();
    if w > total_w {
        header_spans.push(Span::styled(
            " ".repeat(w - total_w),
            Style::default().bg(header_bg),
        ));
    }
    lines.push(Line::from(header_spans));

    let line_count = count_write_lines(content).max(1);
    let line_width = line_count.to_string().len().max(3);
    for (idx, line) in content.lines().enumerate() {
        let formatted = format!("{:<width$} +   {}", idx + 1, line, width = line_width);
        lines.push(make_line_bg(&format!("  {}", formatted), green_fg, add_bg));
    }

    if content.is_empty() {
        let formatted = format!("{:<width$} +   ", 1, width = line_width);
        lines.push(make_line_bg(&format!("  {}", formatted), green_fg, add_bg));
    } else if content.ends_with('\n') {
        let formatted = format!("{:<width$} +   ", line_count, width = line_width);
        lines.push(make_line_bg(&format!("  {}", formatted), green_fg, add_bg));
    }

    lines
}

fn count_write_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.bytes().filter(|b| *b == b'\n').count() + 1
    }
}

fn render_bash(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
    collapsed: bool,
) -> Vec<Line<'static>> {
    if result.is_none() {
        return vec![Line::from(vec![
            Span::styled(
                format!("{} ", spinner()),
                Style::default().fg(Color::Rgb(212, 182, 106)),
            ),
            Span::styled("Bash", Style::default().fg(Color::Rgb(126, 200, 148))),
        ])];
    }

    let bg = Color::Rgb(36, 37, 42);
    let border = Color::Rgb(92, 94, 106);
    let text = Color::Rgb(200, 200, 200);
    let dim = Color::Rgb(140, 142, 150);
    let accent = Color::Rgb(126, 200, 148);

    let border_style = Style::default().fg(border);
    let bg_style = Style::default().bg(bg);

    const MAX_PREVIEW: usize = 9;
    const MAX_EXPANDED_LINES: usize = 200;

    let mut lines: Vec<Line> = Vec::new();

    let mut push_line = |spans: Vec<Span<'static>>| {
        let mut line_spans = vec![Span::styled("\u{2503}", border_style)];
        let mut w = 1;
        for s in &spans {
            w += UnicodeWidthStr::width(&*s.content);
        }
        line_spans.push(Span::raw(" ").style(bg_style));
        w += 1;
        line_spans.extend(spans);
        let pad = content_width.saturating_sub(w);
        if pad > 0 {
            line_spans.push(Span::raw(" ".repeat(pad)).style(bg_style));
        }
        lines.push(Line::from(line_spans));
    };

    let desc = tool_use
        .input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !desc.is_empty() {
        push_line(vec![Span::styled(
            format!("# {}", desc),
            Style::default()
                .fg(dim)
                .bg(bg)
                .add_modifier(Modifier::ITALIC),
        )]);
    }

    let cmd = tool_use
        .input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !cmd.is_empty() {
        push_line(vec![
            Span::styled(
                "$ ",
                Style::default()
                    .fg(accent)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(cmd.to_string(), Style::default().fg(text).bg(bg)),
        ]);
    }

    if let Some(tr) = result
        && !tr.content.is_empty()
    {
        push_line(vec![Span::raw("").style(bg_style)]);
        let out_style = Style::default().fg(Color::Rgb(180, 180, 185)).bg(bg);
        let wrapped = word_wrap(&tr.content, content_width.saturating_sub(2));
        let total = wrapped.len();

        let limit = if collapsed {
            MAX_PREVIEW
        } else {
            MAX_EXPANDED_LINES
        };

        let truncated = total > limit;
        let display: &[String] = if truncated {
            &wrapped[..limit]
        } else {
            &wrapped[..]
        };

        for wl in display {
            push_line(vec![Span::raw(wl.clone()).style(out_style)]);
        }

        if truncated {
            push_line(vec![Span::raw("").style(bg_style)]);
            let indicator_style = Style::default()
                .fg(Color::Rgb(110, 112, 120))
                .bg(bg)
                .add_modifier(Modifier::ITALIC);
            if collapsed {
                push_line(vec![Span::styled(
                    format!(
                        "more output — click to expand ({} more lines)",
                        total - MAX_PREVIEW
                    ),
                    indicator_style,
                )]);
            } else {
                push_line(vec![Span::styled(
                    format!(
                        "output truncated — full content saved to session blocks ({} more lines)",
                        total - MAX_EXPANDED_LINES
                    ),
                    indicator_style,
                )]);
            }
        }
    }

    lines
}
