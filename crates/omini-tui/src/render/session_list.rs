use super::{line_width, pad_display_width, register_and_highlight_lines, truncate_str};
use crate::state::{InteractionStep, UiState};
use crate::types::events::SessionRuntimeState;
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

    // 布局：头部(1) + 内容(fill) + 分隔线(1) + 底部(1)
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

    // ── 头部 ──
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

    // ── 内容 ──
    let mut lines: Vec<Line> = Vec::with_capacity(content_h);
    let mut row_backgrounds: Vec<Option<Color>> = Vec::with_capacity(content_h);

    if total == 0 {
        // 空状态
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

        // 分隔线（空）
        let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
        let mut divider_line = Line::from(Span::styled("─".repeat(content_w), divider_style));
        register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
        frame.render_widget(Paragraph::new(divider_line), divider_area);

        // 底部
        let mut footer_line = Line::from(Span::styled(
            "Esc 返回 · 输入筛选",
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        ));
        register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer_line));
        frame.render_widget(Paragraph::new(footer_line), footer_area);
        return;
    }

    // 滚动计算
    let item_lines = content_h.saturating_sub(2); // reserve top/bottom for indicators
    let mut scroll_off = 0usize;
    if total > item_lines {
        // 保持选中项可见，优先居中
        let ideal = selected.saturating_sub(item_lines / 2);
        scroll_off = ideal.min(total.saturating_sub(item_lines));
    }
    let show_top = scroll_off > 0;
    let show_bot = scroll_off + item_lines < total;

    let max_visible = item_lines.min(total.saturating_sub(scroll_off));
    let time_col_w = 8; // "59分钟前" / "23h前" fit in this column.
    let prefix_w = UnicodeWidthStr::width("❯ ");
    let separator_w = UnicodeWidthStr::width("  ");
    let status_slot_w = 3;
    let max_msg_w = content_w.saturating_sub(prefix_w + time_col_w + separator_w + status_slot_w);

    // ── 构建行 ──
    // 顶部指示器
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

    // 会话条目
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
        let status_marker = session_status_marker(session.runtime_state);
        let msg = truncate_str(&session.title, max_msg_w);
        let used_w = prefix_w
            + time_col_w
            + separator_w
            + status_slot_w
            + UnicodeWidthStr::width(msg.as_str());
        let pad_w = content_w.saturating_sub(used_w);
        let base_style = row_style(fg, bg);

        let mut row_spans = vec![
            Span::styled(prefix.to_string(), base_style),
            Span::styled(time_str, base_style),
            Span::styled("  ", base_style),
            Span::styled(msg, base_style),
            Span::styled(" ".repeat(pad_w), base_style),
        ];
        if let Some((icon, color)) = status_marker {
            row_spans.extend([
                Span::styled(" ", base_style),
                Span::styled(icon, row_style(color, bg)),
                Span::styled(" ", base_style),
            ]);
        } else {
            row_spans.push(Span::styled(" ".repeat(status_slot_w), base_style));
        }
        lines.push(Line::from(row_spans));
        row_backgrounds.push(bg);
    }

    // 填充剩余条目行
    while lines.len() < content_h - 1 {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
        row_backgrounds.push(None);
    }

    // 底部指示器
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

    // ── 分隔线 ──
    let current = selected + 1;
    let indicator = format!(" {}/{} ", current, total);
    let dashes_count = content_w.saturating_sub(indicator.len());
    let divider_line = format!("{}{}", "─".repeat(dashes_count), indicator);
    let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
    let mut divider_line = Line::from(Span::styled(divider_line, divider_style));
    register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
    frame.render_widget(Paragraph::new(divider_line), divider_area);

    // ── 底部 ──
    let mut footer_line = session_footer_line(content_w);
    register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer_line));
    frame.render_widget(Paragraph::new(footer_line), footer_area);
}

fn row_style(fg: Color, bg: Option<Color>) -> Style {
    let style = Style::default().fg(fg);
    if let Some(bg) = bg {
        style.bg(bg)
    } else {
        style
    }
}

fn session_status_marker(
    runtime_state: Option<SessionRuntimeState>,
) -> Option<(&'static str, Color)> {
    match runtime_state {
        None => None,
        Some(SessionRuntimeState::Idle) => Some(("●", Color::Rgb(0x66, 0xbb, 0x6a))),
        Some(SessionRuntimeState::Thinking | SessionRuntimeState::Working) => {
            Some(("●", Color::Rgb(0x64, 0x9f, 0xd5)))
        }
        Some(SessionRuntimeState::Waiting) => Some(("●", Color::Rgb(0xd6, 0x8c, 0x45))),
        Some(SessionRuntimeState::Compacting) => Some(("●", Color::Rgb(0xb0, 0x83, 0xd8))),
    }
}

fn session_footer_line(content_w: usize) -> Line<'static> {
    let base_style = Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a));
    let legend = Line::from(vec![
        Span::styled("↑/↓ 选择 · Enter 确认 · Esc 返回 · 输入筛选 · ", base_style),
        Span::styled(
            "●",
            row_style(
                session_status_marker(Some(SessionRuntimeState::Idle))
                    .expect("idle status has marker")
                    .1,
                None,
            ),
        ),
        Span::styled(" 空闲 ", base_style),
        Span::styled(
            "●",
            row_style(
                session_status_marker(Some(SessionRuntimeState::Working))
                    .expect("working status has marker")
                    .1,
                None,
            ),
        ),
        Span::styled(" 运行 ", base_style),
        Span::styled(
            "●",
            row_style(
                session_status_marker(Some(SessionRuntimeState::Waiting))
                    .expect("waiting status has marker")
                    .1,
                None,
            ),
        ),
        Span::styled(" 等待 ", base_style),
        Span::styled(
            "●",
            row_style(
                session_status_marker(Some(SessionRuntimeState::Compacting))
                    .expect("compacting status has marker")
                    .1,
                None,
            ),
        ),
        Span::styled(" 压缩", base_style),
    ]);

    if line_width(&legend) <= content_w {
        legend
    } else {
        Line::from(Span::styled(
            "↑/↓ 选择 · Enter 确认 · Esc 返回 · 输入筛选",
            base_style,
        ))
    }
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
