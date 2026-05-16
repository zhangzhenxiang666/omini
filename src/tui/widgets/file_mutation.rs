use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use unicode_width::UnicodeWidthStr;

use super::{display_path, spinner, word_wrap};

const MIN_CONTEXT_LINES_TO_COLLAPSE: usize = 4;

enum EditDiffRow {
    Context(String),
    Add(String),
    Delete(String),
}

struct CollapsedEditRowsStyle {
    line_width: usize,
    content_width: usize,
    ctx_bg: Color,
    add_bg: Color,
    del_bg: Color,
    green_fg: Color,
    red_fg: Color,
}

pub(super) fn render_edit(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let file_path = tool_use
        .input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let display_file_path = display_path(file_path, project_dir);
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

    let make_line_bg =
        |text: &str, fg: Color, bg: Color| -> Line<'static> { padded_line_bg(text, fg, bg, w) };

    if let Some(tr) = result
        && tr.is_error
    {
        lines.push(Line::from(vec![
            Span::raw("· "),
            Span::styled("Edit", Style::default().fg(accent)),
            Span::raw(format!(" {}", display_file_path)),
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
    let edit_preview = preview.and_then(|req| match &req.kind {
        ToolPauseKind::Permission(PermissionPreview::Edit(preview)) => Some(preview),
        _ => None,
    });
    let is_permission_preview = result.is_none() && edit_preview.is_some();
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
        if is_permission_preview {
            header_spans.push(Span::styled(
                format!("Matches: {}", replacement_count),
                hdr_accent,
            ));
            header_spans.push(Span::styled(
                format!(" in {} (", display_file_path),
                hdr_plain,
            ));
        } else {
            header_spans.push(Span::styled("Edit", hdr_accent));
            header_spans.push(Span::styled(format!(" {} (", display_file_path), hdr_plain));
        }
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
    let collapsed_edit_rows_style = CollapsedEditRowsStyle {
        line_width,
        content_width: w,
        ctx_bg,
        add_bg,
        del_bg,
        green_fg,
        red_fg,
    };

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
        let mut diff_rows: Vec<EditDiffRow> = Vec::new();
        let mut old_idx = 0;
        let mut new_idx = 0;
        let mut old_line_no = start_line.unwrap_or(1);
        let mut suppress_context_line_no: Option<usize> = None;

        let format_diff_line = |line_no: Option<usize>, marker: char, text: &str| {
            let line_no = line_no
                .map(|n| format!("{:<width$}", n, width = line_width))
                .unwrap_or_else(|| " ".repeat(line_width));
            format!("{line_no} {marker}   {text}")
        };

        while old_idx < old_lines.len() || new_idx < new_lines.len() {
            if old_idx < old_lines.len()
                && new_idx < new_lines.len()
                && old_lines[old_idx] == new_lines[new_idx]
            {
                if suppress_context_line_no == Some(old_line_no) && old_lines[old_idx].is_empty() {
                    old_idx += 1;
                    new_idx += 1;
                    old_line_no += 1;
                    suppress_context_line_no = None;
                    continue;
                }

                let line_no = if suppress_context_line_no == Some(old_line_no) {
                    None
                } else {
                    start_line.map(|_| old_line_no)
                };
                let formatted = format_diff_line(line_no, ' ', old_lines[old_idx]);
                diff_rows.push(EditDiffRow::Context(formatted));
                old_idx += 1;
                new_idx += 1;
                old_line_no += 1;
                suppress_context_line_no = None;
            } else if old_idx < old_lines.len()
                && (new_idx >= new_lines.len()
                    || !new_lines[new_idx..].contains(&old_lines[old_idx]))
            {
                if new_idx < new_lines.len() && !old_lines[old_idx..].contains(&new_lines[new_idx])
                {
                    let del =
                        format_diff_line(start_line.map(|_| old_line_no), '-', old_lines[old_idx]);
                    diff_rows.push(EditDiffRow::Delete(del));

                    let add =
                        format_diff_line(start_line.map(|_| old_line_no), '+', new_lines[new_idx]);
                    diff_rows.push(EditDiffRow::Add(add));

                    old_idx += 1;
                    new_idx += 1;
                    old_line_no += 1;
                    suppress_context_line_no = None;
                } else {
                    let del =
                        format_diff_line(start_line.map(|_| old_line_no), '-', old_lines[old_idx]);
                    diff_rows.push(EditDiffRow::Delete(del));
                    old_idx += 1;
                    old_line_no += 1;
                    suppress_context_line_no = None;
                }
            } else if new_idx < new_lines.len() {
                let add =
                    format_diff_line(start_line.map(|_| old_line_no), '+', new_lines[new_idx]);
                diff_rows.push(EditDiffRow::Add(add));
                new_idx += 1;
                suppress_context_line_no = Some(old_line_no);
            }
        }

        append_collapsed_edit_rows(&mut lines, diff_rows, &collapsed_edit_rows_style);
    }

    lines
}

fn padded_line_bg(text: &str, fg: Color, bg: Color, width: usize) -> Line<'static> {
    let text_w = UnicodeWidthStr::width(text);
    let pad = width.saturating_sub(text_w);
    let style = Style::default().fg(fg).bg(bg);
    let mut spans = vec![Span::styled(text.to_string(), style)];
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
    }
    Line::from(spans)
}

fn append_collapsed_edit_rows(
    lines: &mut Vec<Line<'static>>,
    rows: Vec<EditDiffRow>,
    style: &CollapsedEditRowsStyle,
) {
    let mut idx = 0;
    while idx < rows.len() {
        match &rows[idx] {
            EditDiffRow::Context(_) => {
                let start = idx;
                while idx < rows.len() && matches!(rows[idx], EditDiffRow::Context(_)) {
                    idx += 1;
                }
                let run = &rows[start..idx];
                if run.len() >= MIN_CONTEXT_LINES_TO_COLLAPSE {
                    let marker = format!("{} ⋮", " ".repeat(style.line_width));
                    lines.push(padded_line_bg(
                        &format!("  {}", marker),
                        Color::Reset,
                        style.ctx_bg,
                        style.content_width,
                    ));
                } else {
                    for row in run {
                        if let EditDiffRow::Context(text) = row {
                            lines.push(padded_line_bg(
                                &format!("  {}", text),
                                Color::Reset,
                                style.ctx_bg,
                                style.content_width,
                            ));
                        }
                    }
                }
            }
            EditDiffRow::Add(text) => {
                lines.push(padded_line_bg(
                    &format!("  {}", text),
                    style.green_fg,
                    style.add_bg,
                    style.content_width,
                ));
                idx += 1;
            }
            EditDiffRow::Delete(text) => {
                lines.push(padded_line_bg(
                    &format!("  {}", text),
                    style.red_fg,
                    style.del_bg,
                    style.content_width,
                ));
                idx += 1;
            }
        }
    }
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

pub(super) fn render_write(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let file_path = tool_use
        .input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let display_file_path = display_path(file_path, project_dir);
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
    let write_preview = preview.and_then(|req| match &req.kind {
        ToolPauseKind::Permission(PermissionPreview::Write(preview)) => Some(preview),
        _ => None,
    });
    let is_permission_preview = result.is_none() && write_preview.is_some();
    let mut header_spans: Vec<Span<'static>> = vec![Span::styled("· ", hdr_plain)];
    if is_permission_preview {
        let action = write_preview
            .and_then(|preview| preview.summary.split_once(' ').map(|(action, _)| action))
            .unwrap_or("Update");
        header_spans.push(Span::styled(action.to_string(), hdr_accent));
    } else {
        header_spans.push(Span::styled("Write", hdr_accent));
    }
    header_spans.extend([
        Span::styled(format!(" {} (", display_file_path), hdr_plain),
        Span::styled(format!("+{}", added_lines), hdr_green),
        Span::styled(" -0)", hdr_plain),
    ]);
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
