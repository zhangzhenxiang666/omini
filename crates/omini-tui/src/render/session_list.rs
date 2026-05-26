use super::{pad_display_width, register_and_highlight_lines, truncate_str};
use crate::state::{InteractionStep, UiState};
use chrono::{DateTime, Local, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

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
    let time_col_w = 8; // "59分钟前" / "23h前" fit in this column.
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
        let time_str = pad_display_width(&relative_time(session.updated_at), time_col_w);
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

/// 将 UTC 时间格式化为相对时间（如 "刚刚", "3分钟前", "2h前"）。
pub(super) fn relative_time(utc: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(utc);
    let seconds = duration.num_seconds().max(0);
    if seconds < 60 {
        "刚刚".to_string()
    } else if seconds < 3600 {
        format!("{}分钟前", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h前", seconds / 3600)
    } else if seconds < 604800 {
        format!("{}天前", seconds / 86400)
    } else if seconds < 2592000 {
        format!("{}周前", seconds / 604800)
    } else {
        // 超过一个月显示日期
        utc.with_timezone(&Local).format("%m-%d").to_string()
    }
}
