use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use omini_domain::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use unicode_width::UnicodeWidthStr;

use super::{display_path, tool_error_display_text, tool_title_style, word_wrap};

const MIN_CONTEXT_LINES_TO_COLLAPSE: usize = 3;

/// 解析自 unified diff 文本的 hunk。`old_start` / `new_start` 是
/// 1-based 起始行号;`rows` 按顺序保存每行及其 `LineKind` 和源文本
/// (不包含行首的 ` ` / `+` / `-` 标记)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedHunk {
    pub(crate) old_start: usize,
    pub(crate) new_start: usize,
    pub(crate) rows: Vec<(LineKind, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    Context,
    Add,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HunkRow {
    Context {
        old_line: Option<usize>,
        new_line: Option<usize>,
        text: String,
    },
    Add {
        new_line: usize,
        text: String,
    },
    Delete {
        old_line: usize,
        text: String,
    },
}

pub(crate) fn render_edit(
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

    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let green_fg = Color::Rgb(0x50, 0xc8, 78);
    let red_fg = Color::Rgb(255, 100, 100);
    let add_bg = Color::Rgb(35, 55, 40);
    let del_bg = Color::Rgb(55, 35, 38);
    let ctx_bg = Color::Rgb(40, 44, 52);
    let header_bg = Color::Rgb(38, 42, 50);
    let w = content_width;

    if let Some(tr) = result
        && tr.is_error
    {
        lines.push(Line::from(vec![
            Span::raw("· "),
            Span::styled("Edit", Style::default().fg(accent)),
            Span::raw(format!(" {}", display_file_path)),
        ]));
        let error_style = Style::default().fg(red_fg);
        let display = tool_error_display_text(&tr.content);
        let wrapped = word_wrap(&display, w.saturating_sub(2));
        for wl in wrapped {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wl, error_style),
            ]));
        }
        return lines;
    }

    // 全部走 diff 渲染路径:execute 后用 result.metadata["diff"] 的真实 diff;
    // permission preview 阶段用 EditPermissionPreview.diff 中的 preview diff。
    let diff_text = result_diff_text(result).or_else(|| preview_edit_diff(preview));
    let replacement_count = estimate_replacement_count(result, preview);
    // 解析后立即展开成 HunkRow,并在同一遍里累计 +N/-M,保证 header 与实际渲染一致。
    let mut expanded_hunks: Vec<(ParsedHunk, Vec<HunkRow>)> = Vec::new();
    let mut total_added = 0usize;
    let mut total_removed = 0usize;
    if let Some(text) = diff_text.as_deref() {
        for hunk in parse_unified_diff(text) {
            let rows = expand_hunk_rows(&hunk);
            for r in &rows {
                count_change_row(r, &mut total_added, &mut total_removed);
            }
            expanded_hunks.push((hunk, rows));
        }
    }

    let is_running_without_preview = result.is_none() && preview.is_none();
    let edit_preview = preview.and_then(|req| match &req.kind {
        ToolPauseKind::Permission(PermissionPreview::Edit(preview)) => Some(preview),
        _ => None,
    });
    let is_permission_preview = result.is_none() && edit_preview.is_some();

    if is_running_without_preview {
        lines.push(Line::from(vec![
            Span::raw("· "),
            Span::styled("Edit", tool_title_style(accent, true)),
        ]));
        return lines;
    }

    let hdr_plain = Style::default().bg(header_bg);
    let hdr_accent = Style::default().fg(accent).bg(header_bg);
    let hdr_green = Style::default().fg(green_fg).bg(header_bg);
    let hdr_red = Style::default().fg(red_fg).bg(header_bg);
    let mut header_spans: Vec<Span<'static>> = vec![Span::styled("· ", hdr_plain)];
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
    lines.push(Line::from(header_spans));

    if expanded_hunks.is_empty() {
        return lines;
    }

    let max_line = expanded_hunks
        .iter()
        .flat_map(|(_, rows)| {
            rows.iter().map(|r| match r {
                HunkRow::Delete { old_line, .. } => *old_line,
                HunkRow::Add { new_line, .. } => *new_line,
                HunkRow::Context {
                    old_line, new_line, ..
                } => old_line.unwrap_or(0).max(new_line.unwrap_or(0)),
            })
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let line_width = max_line.to_string().len().max(3);

    for (hunk_idx, (_hunk, rows)) in expanded_hunks.iter().enumerate() {
        if hunk_idx > 0 {
            lines.push(padded_line_bg(
                &format_ellipsis_line(line_width),
                Color::Reset,
                ctx_bg,
                w,
            ));
        }
        render_hunk_rows(
            &mut lines, rows, line_width, w, ctx_bg, add_bg, del_bg, green_fg, red_fg,
        );
    }

    lines
}

fn result_diff_text(result: Option<&ToolResultBlock>) -> Option<String> {
    result
        .and_then(|tr| tr.metadata.as_ref())
        .and_then(|m| m.get("diff"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn preview_edit_diff(preview: Option<&ToolPauseRequest>) -> Option<String> {
    preview.and_then(|req| match &req.kind {
        ToolPauseKind::Permission(PermissionPreview::Edit(preview)) => {
            if preview.diff.is_empty() {
                None
            } else {
                Some(preview.diff.clone())
            }
        }
        _ => None,
    })
}

fn preview_write_diff(preview: Option<&ToolPauseRequest>) -> Option<String> {
    preview.and_then(|req| match &req.kind {
        ToolPauseKind::Permission(PermissionPreview::Write(preview)) => {
            if preview.diff.is_empty() {
                None
            } else {
                Some(preview.diff.clone())
            }
        }
        _ => None,
    })
}

fn estimate_replacement_count(
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
) -> usize {
    if let Some(n) = result
        .and_then(|tr| tr.metadata.as_ref())
        .and_then(|m| m.get("replacement_count"))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
    {
        return n.max(1);
    }
    if let Some(preview) = preview.and_then(|req| match &req.kind {
        ToolPauseKind::Permission(PermissionPreview::Edit(p)) => Some(p),
        _ => None,
    }) {
        return preview.replacement_count.max(1);
    }
    1
}

pub(crate) fn parse_unified_diff(patch: &str) -> Vec<ParsedHunk> {
    let mut hunks: Vec<ParsedHunk> = Vec::new();
    let mut current: Option<ParsedHunk> = None;

    for line in patch.split('\n') {
        if line.starts_with("@@")
            && let Some(h) = current.take()
        {
            hunks.push(h);
        }
        if let Some(header) = line.strip_prefix("@@") {
            // hunk header 形如 `@@ -oldStart[,oldCount] +newStart[,newCount] @@ ...`
            let trimmed = header.split("@@").next().unwrap_or("").trim();
            if let Some((old_part, new_part)) = trimmed.split_once('+') {
                let old_token = old_part.trim().trim_start_matches('-').split(',').next();
                let new_token = new_part.split(',').next();
                let old_start = old_token.and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
                let new_start = new_token.and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
                current = Some(ParsedHunk {
                    old_start,
                    new_start,
                    rows: Vec::new(),
                });
            }
            continue;
        }
        let Some(h) = current.as_mut() else {
            continue;
        };
        if line.starts_with("---") || line.starts_with("+++") {
            // diff 文件头,跳过。
            continue;
        }
        if let Some(text) = line.strip_prefix(' ') {
            h.rows.push((LineKind::Context, text.to_string()));
        } else if let Some(text) = line.strip_prefix('+') {
            h.rows.push((LineKind::Add, text.to_string()));
        } else if let Some(text) = line.strip_prefix('-') {
            h.rows.push((LineKind::Delete, text.to_string()));
        } else {
            // 未知行,忽略。
        }
    }
    if let Some(h) = current {
        hunks.push(h);
    }
    hunks
}

/// 统计单个 HunkRow 对 +N/-M 的贡献。
///
/// `Context { text: "-..." }` / `Context { text: "+..." }` 是配对后的 delete/add 行,
/// 同样要被算进 +N/-M,这样 header 数字与实际渲染行数完全一致。
fn count_change_row(row: &HunkRow, added: &mut usize, removed: &mut usize) {
    match row {
        HunkRow::Add { .. } => *added += 1,
        HunkRow::Delete { .. } => *removed += 1,
        HunkRow::Context { text, .. } => {
            if text.starts_with('-') {
                *removed += 1;
            } else if text.starts_with('+') {
                *added += 1;
            }
        }
    }
}

fn expand_hunk_rows(hunk: &ParsedHunk) -> Vec<HunkRow> {
    let mut out: Vec<HunkRow> = Vec::with_capacity(hunk.rows.len());
    let mut old_line = hunk.old_start;
    let mut new_line = hunk.new_start;
    let mut idx = 0;
    while idx < hunk.rows.len() {
        match hunk.rows[idx].0 {
            LineKind::Context => {
                let (_, text) = &hunk.rows[idx];
                out.push(HunkRow::Context {
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                    text: text.clone(),
                });
                old_line += 1;
                new_line += 1;
                idx += 1;
            }
            LineKind::Delete => {
                // 收集紧邻的 Delete 段。
                let del_start = idx;
                while idx < hunk.rows.len() && hunk.rows[idx].0 == LineKind::Delete {
                    idx += 1;
                }
                let n_del = idx - del_start;
                // 看后面紧跟的 Add 段(若有),与之做 1:1 配对。
                let add_start = idx;
                while idx < hunk.rows.len() && hunk.rows[idx].0 == LineKind::Add {
                    idx += 1;
                }
                let n_add = idx - add_start;
                let n_paired = n_del.min(n_add);
                for i in 0..n_paired {
                    out.push(HunkRow::Context {
                        old_line: Some(old_line + i),
                        new_line: Some(new_line + i),
                        text: format!("-{}", hunk.rows[del_start + i].1),
                    });
                    out.push(HunkRow::Context {
                        old_line: Some(old_line + i),
                        new_line: Some(new_line + i),
                        text: format!("+{}", hunk.rows[add_start + i].1),
                    });
                }
                for i in n_paired..n_del {
                    out.push(HunkRow::Delete {
                        old_line: old_line + i,
                        text: hunk.rows[del_start + i].1.clone(),
                    });
                }
                for i in n_paired..n_add {
                    out.push(HunkRow::Add {
                        new_line: new_line + i,
                        text: hunk.rows[add_start + i].1.clone(),
                    });
                }
                old_line += n_del;
                new_line += n_add;
            }
            LineKind::Add => {
                let (_, text) = &hunk.rows[idx];
                out.push(HunkRow::Add {
                    new_line,
                    text: text.clone(),
                });
                new_line += 1;
                idx += 1;
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn render_hunk_rows(
    lines: &mut Vec<Line<'static>>,
    rows: &[HunkRow],
    line_width: usize,
    content_width: usize,
    ctx_bg: Color,
    add_bg: Color,
    del_bg: Color,
    green_fg: Color,
    red_fg: Color,
) {
    // 渲染规则(permission 面板和 execute 后的实际渲染都走这里,所以两边一致):
    // - 开头/结尾的纯 context(无 +/- 前缀)整段直接丢掉,不画。
    // - 中间夹着的纯 context 段:>= MIN_CONTEXT_LINES_TO_COLLAPSE 行的折叠成单个 "⋮",
    //   短于阈值的原样显示;⋮ 与 +/-/空格 marker 同一列对齐,上下不空行。
    let first_meaningful = rows.iter().position(|r| !is_plain_context(r));
    let last_meaningful = rows.iter().rposition(|r| !is_plain_context(r));
    let (Some(first), Some(last)) = (first_meaningful, last_meaningful) else {
        return;
    };
    let mut idx = first;
    while idx <= last {
        match &rows[idx] {
            HunkRow::Context {
                old_line: _,
                new_line: _,
                text,
            } if text.starts_with('-') || text.starts_with('+') => {
                let marker = if text.starts_with('-') { '-' } else { '+' };
                let trimmed = &text[1..];
                let (old_no, new_no) = match &rows[idx] {
                    HunkRow::Context {
                        old_line, new_line, ..
                    } => (*old_line, *new_line),
                    _ => (None, None),
                };
                let formatted = format_diff_line(line_width, old_no, new_no, marker, trimmed);
                let (fg, bg) = if marker == '-' {
                    (red_fg, del_bg)
                } else {
                    (green_fg, add_bg)
                };
                lines.push(padded_line_bg(
                    &format!("  {formatted}"),
                    fg,
                    bg,
                    content_width,
                ));
                idx += 1;
            }
            HunkRow::Context { .. } => {
                let start = idx;
                while idx <= last && is_plain_context(&rows[idx]) {
                    idx += 1;
                }
                let run_len = idx - start;
                if run_len >= MIN_CONTEXT_LINES_TO_COLLAPSE {
                    lines.push(padded_line_bg(
                        &format_ellipsis_line(line_width),
                        Color::Reset,
                        ctx_bg,
                        content_width,
                    ));
                } else {
                    for row in &rows[start..idx] {
                        if let HunkRow::Context {
                            old_line,
                            new_line,
                            text,
                        } = row
                        {
                            let formatted =
                                format_diff_line(line_width, *old_line, *new_line, ' ', text);
                            lines.push(padded_line_bg(
                                &format!("  {formatted}"),
                                Color::Reset,
                                ctx_bg,
                                content_width,
                            ));
                        }
                    }
                }
            }
            HunkRow::Add { new_line, text } => {
                let formatted = format_diff_line(line_width, None, Some(*new_line), '+', text);
                lines.push(padded_line_bg(
                    &format!("  {formatted}"),
                    green_fg,
                    add_bg,
                    content_width,
                ));
                idx += 1;
            }
            HunkRow::Delete { old_line, text } => {
                let formatted = format_diff_line(line_width, Some(*old_line), None, '-', text);
                lines.push(padded_line_bg(
                    &format!("  {formatted}"),
                    red_fg,
                    del_bg,
                    content_width,
                ));
                idx += 1;
            }
        }
    }
}

/// 是否是"纯 context"行(HunkRow::Context 且 text 不带 -/+ 前缀)。
/// 这类行代表源文件/目标文件都存在的不变行,可以折叠或丢弃。
fn is_plain_context(row: &HunkRow) -> bool {
    matches!(
        row,
        HunkRow::Context { text, .. } if !text.starts_with('-') && !text.starts_with('+')
    )
}

/// 构造 `⋮` 行:与 `format_diff_line` 输出的 marker 列对齐(2 字符缩进 +
/// `2*line_width + 1` 个空格 + 一个显式空格,再放 `⋮`),让 `⋮` 前面有一个
/// 字面量上的 prefix 空格(对应 marker 列前的那个分隔空格),不留上下空行。
fn format_ellipsis_line(line_width: usize) -> String {
    format!("  {} ⋮", " ".repeat(2 * line_width + 1))
}

fn format_diff_line(
    line_width: usize,
    old_no: Option<usize>,
    new_no: Option<usize>,
    marker: char,
    text: &str,
) -> String {
    let old_str = old_no
        .map(|n| format!("{:<width$}", n, width = line_width))
        .unwrap_or_else(|| " ".repeat(line_width));
    let new_str = new_no
        .map(|n| format!("{:<width$}", n, width = line_width))
        .unwrap_or_else(|| " ".repeat(line_width));
    format!("{old_str} {new_str} {marker}   {text}")
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

pub(crate) fn render_write(
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

    let accent = Color::Rgb(0x42, 0xb3, 0xc2);
    let green_fg = Color::Rgb(0x50, 0xc8, 78);
    let red_fg = Color::Rgb(255, 100, 100);
    let add_bg = Color::Rgb(35, 55, 40);
    let header_bg = Color::Rgb(38, 42, 50);
    let w = content_width;

    if let Some(tr) = result
        && tr.is_error
    {
        lines.push(Line::from(vec![
            Span::raw("· "),
            Span::styled("Write", Style::default().fg(accent)),
        ]));
        let error_style = Style::default().fg(red_fg);
        let display = tool_error_display_text(&tr.content);
        let wrapped = word_wrap(&display, w.saturating_sub(2));
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
            Span::raw("· "),
            Span::styled("Write", tool_title_style(accent, true)),
        ]));
        return lines;
    }

    // Write 工具的 diff 来源优先级:execute 后 metadata["diff"] > preview.diff。
    // 两者都是新格式(prepare/execute 都填),不再走"枚举 content 行"的兼容路径。
    let diff_text = result_diff_text(result).or_else(|| preview_write_diff(preview));
    // 解析后立即展开成 HunkRow,并在同一遍里累计 +N/-M,保证 header 与实际渲染一致。
    let mut expanded_hunks: Vec<(ParsedHunk, Vec<HunkRow>)> = Vec::new();
    let mut added_lines = 0usize;
    let mut removed_lines = 0usize;
    if let Some(text) = diff_text.as_deref() {
        for hunk in parse_unified_diff(text) {
            let rows = expand_hunk_rows(&hunk);
            for r in &rows {
                count_change_row(r, &mut added_lines, &mut removed_lines);
            }
            expanded_hunks.push((hunk, rows));
        }
    }

    let write_preview = preview.and_then(|req| match &req.kind {
        ToolPauseKind::Permission(PermissionPreview::Write(preview)) => Some(preview),
        _ => None,
    });
    let is_permission_preview = result.is_none() && write_preview.is_some();
    let mut header_spans: Vec<Span<'static>> =
        vec![Span::styled("· ", Style::default().bg(header_bg))];
    if is_permission_preview {
        let action = write_preview
            .and_then(|preview| preview.summary.split_once(' ').map(|(action, _)| action))
            .unwrap_or("Update");
        header_spans.push(Span::styled(
            action.to_string(),
            Style::default().fg(accent).bg(header_bg),
        ));
    } else {
        header_spans.push(Span::styled(
            "Write",
            Style::default().fg(accent).bg(header_bg),
        ));
    }
    header_spans.extend([
        Span::styled(
            format!(" {} (", display_file_path),
            Style::default().bg(header_bg),
        ),
        Span::styled(
            format!("+{}", added_lines),
            Style::default().fg(green_fg).bg(header_bg),
        ),
        Span::styled(
            format!(" -{}", removed_lines),
            Style::default().bg(header_bg),
        ),
        Span::styled(")", Style::default().bg(header_bg)),
    ]);
    let total_w: usize = header_spans.iter().map(|s| s.width()).sum();
    if w > total_w {
        header_spans.push(Span::styled(
            " ".repeat(w - total_w),
            Style::default().bg(header_bg),
        ));
    }
    lines.push(Line::from(header_spans));

    if expanded_hunks.is_empty() {
        // 空文件占位:在第 1 行显示一个 "+" 行。
        let formatted = format_diff_line(3, None, Some(1), '+', "");
        lines.push(padded_line_bg(
            &format!("  {formatted}"),
            green_fg,
            add_bg,
            w,
        ));
        return lines;
    }

    let max_line = expanded_hunks
        .iter()
        .flat_map(|(_, rows)| {
            rows.iter().map(|r| match r {
                HunkRow::Delete { old_line, .. } => *old_line,
                HunkRow::Add { new_line, .. } => *new_line,
                HunkRow::Context {
                    old_line, new_line, ..
                } => old_line.unwrap_or(0).max(new_line.unwrap_or(0)),
            })
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let line_width = max_line.to_string().len().max(3);

    let ctx_bg = Color::Rgb(40, 44, 52);
    for (hunk_idx, (_hunk, rows)) in expanded_hunks.iter().enumerate() {
        if hunk_idx > 0 {
            lines.push(padded_line_bg(
                &format_ellipsis_line(line_width),
                Color::Reset,
                ctx_bg,
                w,
            ));
        }
        render_hunk_rows(
            &mut lines,
            rows,
            line_width,
            w,
            ctx_bg,
            add_bg,
            Color::Rgb(55, 35, 38),
            green_fg,
            red_fg,
        );
    }

    lines
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::{
        EditPermissionPreview, PermissionPreview, ToolPauseKind, ToolPauseRequest,
    };
    use omini_domain::message::{ToolResultBlock, ToolUseBlock};
    use serde_json::{Map, Value};
    use std::collections::HashMap;

    fn edit_tool_use(old: &str, new: &str, replace_all: bool) -> ToolUseBlock {
        let mut input = HashMap::new();
        input.insert(
            "file_path".to_string(),
            Value::String("/repo/file.txt".to_string()),
        );
        input.insert("old_string".to_string(), Value::String(old.to_string()));
        input.insert("new_string".to_string(), Value::String(new.to_string()));
        input.insert("replace_all".to_string(), Value::Bool(replace_all));
        ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "edit".to_string(),
            input,
        }
    }

    fn tool_result_with_diff(diff: &str) -> ToolResultBlock {
        let mut metadata = Map::new();
        metadata.insert("diff".to_string(), Value::String(diff.to_string()));
        metadata.insert("replacement_count".to_string(), Value::Number(1.into()));
        ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: false,
            content: "ok".to_string(),
            metadata: Some(metadata),
        }
    }

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn parse_unified_diff_extracts_hunk_header_and_rows() {
        let patch = "--- a\n+++ b\n@@ -1,3 +1,3 @@\n keep\n-deleted\n+added\n keep2\n";
        let hunks = parse_unified_diff(patch);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[0].rows.len(), 4);
        assert_eq!(hunks[0].rows[0].0, LineKind::Context);
        assert_eq!(hunks[0].rows[0].1, "keep");
        assert_eq!(hunks[0].rows[1].0, LineKind::Delete);
        assert_eq!(hunks[0].rows[1].1, "deleted");
        assert_eq!(hunks[0].rows[2].0, LineKind::Add);
        assert_eq!(hunks[0].rows[2].1, "added");
        assert_eq!(hunks[0].rows[3].0, LineKind::Context);
        assert_eq!(hunks[0].rows[3].1, "keep2");
    }

    #[test]
    fn render_edit_uses_unified_diff_when_metadata_has_diff() {
        let tool_use = edit_tool_use("a", "b", false);
        let diff = "--- a\n+++ a\n@@ -3 +3 @@\n-a\n+b\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        let joined = text.join("\n");
        // header 反映从 diff 解析出的总计。
        assert!(joined.contains("+1 -1"), "{joined}");
        // header 显示文件路径。
        assert!(joined.contains("Edit /repo/file.txt"), "{joined}");
        // 删除/新增行共享同一对新旧行号。
        assert!(joined.contains("  3   3   -   a"), "{joined}");
        assert!(joined.contains("  3   3   +   b"), "{joined}");
    }

    #[test]
    fn render_edit_assigns_correct_old_and_new_line_numbers_from_hunk_header() {
        let tool_use = edit_tool_use("ctx", "ctx2", false);
        // hunk header 形如 -10,3 +12,4 —— old 从第 10 行起,new 从第 12 行起。
        let diff =
            "--- a\n+++ a\n@@ -10,3 +12,4 @@\n line1\n-line2\n+line2-rewritten\n+extra\n line3\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 100, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // 旧的 "line2" 和新的 "line2-rewritten" 都落在 11/13。
        assert!(
            text.iter().any(|l| l.contains(" 11  13  -   line2")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|l| l.contains(" 11  13  +   line2-rewritten")),
            "{text:?}"
        );
        // 新增的 "extra" 行展示新的第 14 行。
        assert!(
            text.iter().any(|l| l.contains(" 14  +   extra")),
            "{text:?}"
        );
    }

    #[test]
    fn render_edit_handles_mixed_delete_add_with_same_line_numbers() {
        let tool_use = edit_tool_use("x", "y", false);
        // 纯修改会在同一逻辑位置产出 delete+add;渲染时删除行和新增行
        // 都应展示同一个 new-line 号。
        let diff = "--- a\n+++ a\n@@ -5 +5 @@\n-old\n+new\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        let delete_row = text
            .iter()
            .find(|l| l.contains(" -   old"))
            .expect("delete row");
        let add_row = text
            .iter()
            .find(|l| l.contains(" +   new"))
            .expect("add row");
        assert!(delete_row.contains(" 5   5"), "{delete_row}");
        assert!(add_row.contains(" 5   5"), "{add_row}");
    }

    #[test]
    fn render_edit_falls_back_to_start_lines_when_diff_missing() {
        let tool_use = edit_tool_use("a", "b", false);
        let preview = ToolPauseRequest {
            tool_use_id: "toolu_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "edit".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Edit(EditPermissionPreview {
                summary: "Edit".to_string(),
                path: "/repo/file.txt".to_string(),
                replacement_count: 1,
                diff: "@@ -7 +7 @@\n-a\n+b\n".to_string(),
            })),
        };
        let lines = render_edit(&tool_use, None, Some(&preview), 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert!(
            text.iter().any(|l| l.contains(" 7   7   -   a")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains(" 7   7   +   b")),
            "{text:?}"
        );
    }

    #[test]
    fn render_edit_handles_multiple_hunks_with_increasing_line_numbers() {
        let tool_use = edit_tool_use("ctx", "ctx2", false);
        // 两个 hunk 处于不同位置:hunk1 从 -1 +1 起步,hunk2 从
        // -20 +22 (hunk1 新增 2 行,把 new_start 推到了 22)。
        let diff = "--- a\n+++ a\n@@ -1 +1 @@\n-x\n+X\n@@ -20 +22 @@\n-y\n+Y\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 100, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // header 反映 +2 -2。
        assert!(text[0].contains("+2 -2"), "{:?}", text[0]);
        // 两个 hunk 都应渲染。
        assert!(text.iter().any(|l| l.contains("+   X")), "{text:?}");
        assert!(text.iter().any(|l| l.contains("+   Y")), "{text:?}");
        // 两个 hunk 之间应存在分隔符。
        assert!(text.iter().any(|l| l.contains("⋮")), "{text:?}");
    }

    #[test]
    fn render_edit_pairs_all_consecutive_deletes_and_adds_one_to_one() {
        // 4 行被替换为 4 行(全删 + 全加)。所有 8 行都应被 1:1 配对,
        // 不会出现"只配最后一对"的边界 bug。
        let tool_use = edit_tool_use("old", "new", false);
        let diff = "--- a\n+++ a\n@@ -289,4 +289,4 @@\n-命令行模式：\n-1. 从 stdin...\n-2. 通过 --query\n-3. 输出排序结果\n+CLI interface:\n+1. Read...\n+2. Specify...\n+3. Print...\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 100, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // header 与 4 + 4 一致。
        assert!(text[0].contains("+4 -4"), "{:?}", text[0]);
        // 4 对配对行,每对都共享同一对新旧行号。
        // 行布局: `  <old> <new> <marker>   <text>`(`format_diff_line` 的格式)。
        for (old, new, marker, body) in [
            (289, 289, '-', "命令行模式："),
            (290, 290, '-', "1. 从 stdin..."),
            (291, 291, '-', "2. 通过 --query"),
            (292, 292, '-', "3. 输出排序结果"),
            (289, 289, '+', "CLI interface:"),
            (290, 290, '+', "1. Read..."),
            (291, 291, '+', "2. Specify..."),
            (292, 292, '+', "3. Print..."),
        ] {
            let needle = format!("  {old} {new} {marker}   {body}");
            assert!(
                text.iter().any(|l| l.contains(&needle)),
                "missing row {needle:?} in {text:?}"
            );
        }
        // 不应再有"new=289 配到 old=292"这种错位的配对行。
        assert!(
            !text.iter().any(|l| l.contains(" 292 289  -")),
            "stale paired line: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains(" 292 289  +")),
            "stale paired line: {text:?}"
        );
    }

    #[test]
    fn render_edit_pairing_handles_unbalanced_delete_and_add_runs() {
        // 3 个 delete 配 5 个 add:前 3 对配对,后 2 个 add 走 excess 分支(只显示 new_line)。
        let tool_use = edit_tool_use("old", "new", false);
        let diff = "--- a\n+++ a\n@@ -10,3 +10,5 @@\n-d1\n-d2\n-d3\n+a1\n+a2\n+a3\n+a4\n+a5\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 100, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // header 反映 +5 -3。
        assert!(text[0].contains("+5 -3"), "{:?}", text[0]);
        // 前 3 对配对。
        for (old, new) in [(10, 10), (11, 11), (12, 12)] {
            assert!(
                text.iter()
                    .any(|l| l.contains(&format!("  {old}  {new}  -"))),
                "{text:?}"
            );
            assert!(
                text.iter()
                    .any(|l| l.contains(&format!("  {old}  {new}  +"))),
                "{text:?}"
            );
        }
        // 后 2 个 add 单独显示,只带 new_line。
        assert!(
            text.iter().any(|l| l.contains("      13  +   a4")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("      14  +   a5")),
            "{text:?}"
        );
    }

    #[test]
    fn render_edit_skips_leading_and_trailing_context_lines() {
        // 1 行 leading context + 1 行 delete + 1 行 add + 1 行 trailing context。
        // 开头/结尾的纯 context 整段丢弃,只有中间的 change 留下。
        let tool_use = edit_tool_use("ctx", "ctx2", false);
        let diff = "--- a\n+++ a\n@@ -10,3 +10,3 @@\n ctx1\n-old\n+new\n ctx2\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // 变化行(配对的 delete+add)应被渲染。
        assert!(text.iter().any(|l| l.contains(" -   old")), "{text:?}");
        assert!(text.iter().any(|l| l.contains(" +   new")), "{text:?}");
        // 开头/结尾的 context 行(以空格 marker 开头)不应出现。
        assert!(
            !text.iter().any(|l| l.contains(" ctx1")),
            "leading context should be hidden: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains(" ctx2")),
            "trailing context should be hidden: {text:?}"
        );
    }

    #[test]
    fn render_edit_collapses_long_middle_context_to_ellipsis() {
        // 3 行 leading context + 1 对 change + 7 行 middle context + 1 对 change + 3 行 trailing。
        // leading/trailing 整段丢弃;7 行 middle 折叠成单个 ⋮;两对 change 都在。
        let tool_use = edit_tool_use("ctx", "ctx2", false);
        let diff = "--- a\n+++ a\n@@ -1 +1 @@\n a1\n a2\n a3\n-old1\n+new1\n c1\n c2\n c3\n c4\n c5\n c6\n c7\n-old2\n+new2\n d1\n d2\n d3\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 100, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // 两对 change 都保留。
        assert!(text.iter().any(|l| l.contains(" -   old1")), "{text:?}");
        assert!(text.iter().any(|l| l.contains(" +   new1")), "{text:?}");
        assert!(text.iter().any(|l| l.contains(" -   old2")), "{text:?}");
        assert!(text.iter().any(|l| l.contains(" +   new2")), "{text:?}");
        // leading a1..a3、trailing d1..d3 不出现。
        for ctx in [" a1", " a2", " a3", " d1", " d2", " d3"] {
            assert!(
                !text.iter().any(|l| l.contains(ctx)),
                "edge context {ctx:?} should be hidden: {text:?}"
            );
        }
        // 中段 c1..c7(7 行)折叠成单个 ⋮,c 行也都不出现。
        for ctx in [" c1", " c2", " c3", " c4", " c5", " c6", " c7"] {
            assert!(
                !text.iter().any(|l| l.contains(ctx)),
                "collapsed context {ctx:?} should not appear: {text:?}"
            );
        }
        let ellipsis_rows: Vec<&String> = text.iter().filter(|l| l.contains('⋮')).collect();
        assert_eq!(
            ellipsis_rows.len(),
            1,
            "expected exactly one ⋮ marker in middle: {text:?}"
        );
        // ⋮ 行与 + / - marker 列对齐(跳过头部带 (+N -M) 的行,找到真正的 change 行)。
        let marker_col = text
            .iter()
            .skip(1)
            .find_map(|l| l.find('+').or_else(|| l.find('-')))
            .expect("some change row");
        let ellipsis_col = ellipsis_rows[0].find('⋮').unwrap();
        assert_eq!(marker_col, ellipsis_col, "⋮ not aligned with markers");
    }

    #[test]
    fn render_edit_collapses_three_or_more_middle_context_lines() {
        // 2 行 middle context 不折叠(短于阈值),3 行折叠成 ⋮。
        let tool_use = edit_tool_use("x", "y", false);

        // 2 行:应原样展开。
        let diff_short = "--- a\n+++ a\n@@ -1,2 +1,2 @@\n-a\n+b1\n b2\n-x\n+y\n";
        let result = tool_result_with_diff(diff_short);
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert!(text.iter().any(|l| l.contains(" b1")), "{text:?}");
        assert!(text.iter().any(|l| l.contains(" b2")), "{text:?}");
        assert!(!text.iter().any(|l| l.contains('⋮')), "{text:?}");

        // 3 行:折叠成单个 ⋮。
        let diff_long = "--- a\n+++ a\n@@ -1,5 +1,5 @@\n-a\n+b1\n b2\n b3\n b4\n-x\n+y\n";
        let result = tool_result_with_diff(diff_long);
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        let ellipsis_rows: Vec<&String> = text.iter().filter(|l| l.contains('⋮')).collect();
        assert_eq!(
            ellipsis_rows.len(),
            1,
            "expected exactly one ⋮ marker: {text:?}"
        );
        // 中间夹着的 3 行 context (b2/b3/b4) 应当被折叠掉,只剩 ⋮;而配对 add (b1) 仍要出现。
        for ctx in [" b2", " b3", " b4"] {
            assert!(
                !text.iter().any(|l| l.contains(ctx)),
                "collapsed context {ctx:?} should not appear: {text:?}"
            );
        }
        assert!(
            text.iter().any(|l| l.contains("+   b1")),
            "paired add b1 should still appear: {text:?}"
        );
    }

    #[test]
    fn render_edit_hunk_separator_is_single_ellipsis_aligned_with_markers() {
        // 两个 hunk 中间没有空行,只有单个 ⋮,且与 marker 列对齐。
        let tool_use = edit_tool_use("ctx", "ctx2", false);
        let diff = "--- a\n+++ a\n@@ -1 +1 @@\n-x\n+X\n@@ -50 +50 @@\n-y\n+Y\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // 只应有一个 ⋮ 行(没有上下空行)。
        let ellipsis_rows: Vec<&String> = text.iter().filter(|l| l.contains('⋮')).collect();
        assert_eq!(ellipsis_rows.len(), 1, "{text:?}");
        // ⋮ 不应是空行。
        assert!(ellipsis_rows[0].trim() == "⋮", "got {:?}", ellipsis_rows[0]);
    }

    #[test]
    fn render_edit_ellipsis_line_has_literal_space_before_symbol() {
        // 折叠/分隔的 ⋮ 行必须有一个字面量空格紧挨在 ⋮ 前面(对应 marker 列前
        // 的那个分隔空格),而不是只有一堆重复空格串到 ⋮。
        let tool_use = edit_tool_use("a", "b", false);
        // leading(1) + change(2: −a +b) + middle(5 context) + change(2: −c +d) + trailing(1)
        // 5 行 middle 满足 ≥3 触发折叠。
        let diff = "--- a\n+++ a\n@@ -1,9 +1,9 @@\n leading\n-a\n+b\n c1\n c2\n c3\n c4\n c5\n-c\n+d\n trailing\n";
        let result = tool_result_with_diff(diff);
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        let ellipsis_rows: Vec<&String> = text.iter().filter(|l| l.contains('⋮')).collect();
        assert_eq!(ellipsis_rows.len(), 1, "{text:?}");
        let row = ellipsis_rows[0].trim_end();
        assert!(
            row.ends_with(" ⋮"),
            "ellipsis 行紧邻 ⋮ 之前必须有一个空格,实际: {row:?}"
        );
    }

    #[test]
    fn render_edit_handles_missing_diff_with_replacement_count() {
        // 没有 diff(无 execute 后 metadata["diff"]、也无 preview.diff)时,
        // 只渲染 header,不能伪造出错误的行号。replacement_count 来自 metadata。
        let tool_use = edit_tool_use("a", "b", true);
        let mut metadata = Map::new();
        metadata.insert("replacement_count".to_string(), Value::Number(3.into()));
        let result = ToolResultBlock {
            tool_use_id: "toolu_1".to_string(),
            is_error: false,
            content: "ok".to_string(),
            metadata: Some(metadata),
        };
        let lines = render_edit(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        // header 还是渲染,但 +N/-M 都为 0(因为没有 diff)
        assert!(text[0].contains("Edit /repo/file.txt"));
        assert!(text[0].contains("+0 -0"));
    }

    #[test]
    fn render_write_uses_diff_metadata_when_present() {
        let mut input = HashMap::new();
        input.insert(
            "file_path".to_string(),
            Value::String("/repo/new.txt".to_string()),
        );
        input.insert(
            "content".to_string(),
            Value::String("alpha\nbeta\n".to_string()),
        );
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "write".to_string(),
            input,
        };
        let diff = "--- new.txt\n+++ new.txt\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n";
        let result = tool_result_with_diff(diff);
        let lines = render_write(&tool_use, Some(&result), None, 80, None);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert!(text[0].contains("+2"), "{:?}", text[0]);
        assert!(text.iter().any(|l| l.contains("+   alpha")), "{text:?}");
        assert!(text.iter().any(|l| l.contains("+   beta")), "{text:?}");
    }
}
