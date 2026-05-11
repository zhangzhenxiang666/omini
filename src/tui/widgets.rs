use crate::tools::apply_patch_tool::{PatchOp, parse_patch};
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
    content_width: usize,
    collapsed: bool,
) -> Vec<Line<'static>> {
    match tool_use.name.as_str() {
        "bash" | "execute_command" | "run_terminal_cmd" | "run_command" => {
            render_bash(tool_use, tool_result, content_width, collapsed)
        }
        "read" => render_read(tool_use, tool_result, content_width),
        "apply_patch" => render_apply_patch(tool_use, tool_result, content_width),
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

fn render_apply_patch(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let input_str = tool_use
        .input
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if input_str.is_empty() {
        lines.push(Line::from(Span::styled(
            "Missing patch input",
            Style::default().fg(Color::Rgb(255, 100, 100)),
        )));
        return lines;
    }

    let ops = match parse_patch(input_str) {
        Ok(ops) => ops,
        Err(e) => {
            lines.push(Line::from(Span::styled(
                format!("Patch parse error: {e}"),
                Style::default().fg(Color::Rgb(255, 100, 100)),
            )));
            return lines;
        }
    };

    // ── Colors ──
    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let green_fg = Color::Rgb(0x50, 0xc8, 0x78);
    let red_fg = Color::Rgb(255, 100, 100);
    // Background tints
    let add_bg = Color::Rgb(35, 55, 40);
    let del_bg = Color::Rgb(55, 35, 38);
    let ctx_bg = Color::Rgb(40, 44, 52);
    let header_bg = Color::Rgb(38, 42, 50);

    let w = content_width;

    // Helper: single-colored padded line with bg
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

    // Helper: multi-span padded line with background
    let make_padded = |spans: Vec<Span<'static>>, bg: Color| -> Line<'static> {
        let total_w: usize = spans.iter().map(|s| s.width()).sum();
        let pad = w.saturating_sub(total_w);
        let mut result = spans;
        if pad > 0 {
            result.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        }
        Line::from(result)
    };

    // ── Operations ──
    for (op_idx, op) in ops.iter().enumerate() {
        if op_idx > 0 {
            lines.push(Line::from(""));
        }

        match op {
            PatchOp::Add { path, content } => {
                // "· Write /path (+N ↑)"
                let added = content.len();
                let mut header_spans: Vec<Span<'static>> = Vec::new();
                header_spans.push(Span::styled("· ", Style::default().bg(header_bg)));
                header_spans.push(Span::styled(
                    "Write",
                    Style::default().fg(accent).bg(header_bg),
                ));
                header_spans.push(Span::styled(
                    format!(" {} (", path),
                    Style::default().bg(header_bg),
                ));
                header_spans.push(Span::styled(
                    format!("+{}", added),
                    Style::default().fg(green_fg).bg(header_bg),
                ));
                header_spans.push(Span::styled(")", Style::default().bg(header_bg)));
                lines.push(make_padded(header_spans, header_bg));

                // All-+ diff view with line numbers from 1
                for (i, line) in content.iter().enumerate() {
                    let line_num = i + 1;
                    let formatted = format!("{:>5} +   {}", line_num, line);
                    lines.push(make_line_bg(&format!("  {}", formatted), green_fg, add_bg));
                }
            }

            PatchOp::Update {
                path,
                new_path,
                hunks,
            } => {
                // ── Count added/removed across all hunks ──
                let mut total_added = 0usize;
                let mut total_removed = 0usize;
                for hunk in hunks {
                    let mut i = 0;
                    let mut j = 0;
                    while i < hunk.before.len() || j < hunk.after.len() {
                        if i < hunk.before.len()
                            && j < hunk.after.len()
                            && hunk.before[i] == hunk.after[j]
                        {
                            i += 1;
                            j += 1;
                        } else if i < hunk.before.len()
                            && (j >= hunk.after.len()
                                || !hunk.after[j..].iter().any(|a| a == &hunk.before[i]))
                        {
                            if j < hunk.after.len()
                                && !hunk.before[i..].iter().any(|b| b == &hunk.after[j])
                            {
                                // Replace pair
                                total_added += 1;
                                total_removed += 1;
                                i += 1;
                                j += 1;
                            } else {
                                // Pure delete
                                total_removed += 1;
                                i += 1;
                            }
                        } else {
                            // Pure add
                            total_added += 1;
                            j += 1;
                        }
                    }
                }

                // ── Compute dynamic line-number width across all hunks ──
                let max_line_num = hunks
                    .iter()
                    .map(|h| {
                        let last_before = h.line_anchor.saturating_add(h.before.len());
                        let last_after = h.line_anchor.saturating_add(h.after.len());
                        last_before.max(last_after).saturating_sub(1)
                    })
                    .max()
                    .unwrap_or(0);
                let line_width = max_line_num.max(1).to_string().len().max(3);

                // ── Header: "  · Edit /path (+N ↑, -M ↓)" ──
                let mut header_spans: Vec<Span<'static>> = Vec::new();
                let hdr_plain = Style::default().bg(header_bg);
                let hdr_fg = |c: Color| Style::default().fg(c).bg(header_bg);

                header_spans.push(Span::styled("· ", hdr_plain));
                header_spans.push(Span::styled("Edit", hdr_fg(accent)));

                if let Some(np) = new_path {
                    header_spans.push(Span::styled(format!(" {} → {} (", path, np), hdr_plain));
                } else {
                    header_spans.push(Span::styled(format!(" {} (", path), hdr_plain));
                }
                header_spans.push(Span::styled(format!("+{}", total_added), hdr_fg(green_fg)));
                header_spans.push(Span::styled(", ", hdr_plain));
                header_spans.push(Span::styled(format!("-{}", total_removed), hdr_fg(red_fg)));
                header_spans.push(Span::styled(")", hdr_plain));

                lines.push(make_padded(header_spans, header_bg));

                // ── Hunks ──
                for (hunk_idx, hunk) in hunks.iter().enumerate() {
                    if hunk_idx > 0 {
                        lines.push(make_line_bg("   ...", Color::Reset, ctx_bg));
                    }

                    // ── Diff walk ──
                    let mut i = 0;
                    let mut j = 0;
                    let mut orig_line = hunk.line_anchor;

                    while i < hunk.before.len() || j < hunk.after.len() {
                        if i < hunk.before.len()
                            && j < hunk.after.len()
                            && hunk.before[i] == hunk.after[j]
                        {
                            // Context line
                            let formatted = format!(
                                "{:>width$} {}",
                                orig_line,
                                hunk.before[i],
                                width = line_width
                            );
                            lines.push(make_line_bg(
                                &format!("  {}", formatted),
                                Color::Reset,
                                ctx_bg,
                            ));
                            i += 1;
                            j += 1;
                            orig_line += 1;
                        } else if i < hunk.before.len()
                            && (j >= hunk.after.len()
                                || !hunk.after[j..].iter().any(|a| a == &hunk.before[i]))
                        {
                            // before[i] has no match in after[j..] → delete or replace
                            if j < hunk.after.len()
                                && !hunk.before[i..].iter().any(|b| b == &hunk.after[j])
                            {
                                // Replace pair: output delete then add sharing line number
                                let del = format!(
                                    "{:>width$} -   {}",
                                    orig_line,
                                    hunk.before[i],
                                    width = line_width
                                );
                                lines.push(make_line_bg(&format!("  {}", del), red_fg, del_bg));
                                let add = format!(
                                    "{:>width$} +   {}",
                                    orig_line,
                                    hunk.after[j],
                                    width = line_width
                                );
                                lines.push(make_line_bg(&format!("  {}", add), green_fg, add_bg));
                                i += 1;
                                j += 1;
                                orig_line += 1;
                            } else {
                                // Pure delete
                                let del = format!(
                                    "{:>width$} -   {}",
                                    orig_line,
                                    hunk.before[i],
                                    width = line_width
                                );
                                lines.push(make_line_bg(&format!("  {}", del), red_fg, del_bg));
                                i += 1;
                                orig_line += 1;
                            }
                        } else {
                            // Pure add: before[i] matches later in after[j..]
                            let add =
                                format!("{:>width$} +   {}", "", hunk.after[j], width = line_width);
                            lines.push(make_line_bg(&format!("  {}", add), green_fg, add_bg));
                            j += 1;
                        }
                    }
                }
            }

            PatchOp::Delete { path } => {
                let mut header_spans: Vec<Span<'static>> = Vec::new();
                header_spans.push(Span::styled("· ", Style::default().bg(header_bg)));
                header_spans.push(Span::styled(
                    "Delete",
                    Style::default().fg(red_fg).bg(header_bg),
                ));
                header_spans.push(Span::styled(
                    format!(" {}", path),
                    Style::default().bg(header_bg),
                ));
                lines.push(make_padded(header_spans, header_bg));
            }
        }
    }

    if let Some(tr) = result
        && tr.is_error
    {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        let wrapped = word_wrap(&tr.content, w.saturating_sub(2));
        for wl in wrapped {
            lines.push(make_line_bg(&wl, red_fg, Color::Rgb(55, 30, 30)));
        }
    }

    lines
}
fn render_bash(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    content_width: usize,
    collapsed: bool,
) -> Vec<Line<'static>> {
    let bg = Color::Rgb(36, 37, 42);
    let border = Color::Rgb(92, 94, 106);
    let text = Color::Rgb(200, 200, 200);
    let dim = Color::Rgb(140, 142, 150);
    let accent = Color::Rgb(126, 200, 148);
    let running = Color::Rgb(212, 182, 106);

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

    match result {
        None => {
            let spin = spinner();
            push_line(vec![Span::styled(
                format!("{} Running...", spin),
                Style::default()
                    .fg(running)
                    .bg(bg)
                    .add_modifier(Modifier::ITALIC),
            )]);
        }
        Some(tr) => {
            if !tr.content.is_empty() {
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
        }
    }

    lines
}
