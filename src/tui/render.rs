use crate::tui::state::{InteractionStep, ModelSelectionEntry, UiState};
use crate::tui::widgets::{
    build_bordered_lines, build_plain_lines, build_thinking_lines, render_tool,
};
use crate::types::message::ContentBlock;
use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use std::collections::{HashMap, HashSet};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    let area = frame.area();

    // 会话列表：全屏模式（替换整个界面）
    if let Some(InteractionStep::Session { .. }) = &state.interaction_step {
        render_session_list(state, frame, area);
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    state.messages_area = chunks[2];

    render_header(state, frame, chunks[0]);
    render_messages(state, frame, chunks[2]);
    render_autocomplete(state, frame, chunks[4]);
    render_input(state, frame, chunks[4]);
    render_footer(state, frame, chunks[5]);

    // 模型选择等弹窗：覆盖在正常布局之上（不遮盖消息区背景）
    if state.interaction_request.is_some() {
        render_interaction(state, frame, area);
    }
}

fn render_autocomplete(state: &UiState, frame: &mut ratatui::Frame, input_area: Rect) {
    if !state.autocomplete.visible || state.autocomplete.filtered.is_empty() {
        return;
    }

    let max_items = 6;
    let count = state.autocomplete.filtered.len().min(max_items);
    let popup_width = input_area.width;

    let popup_height = count as u16;
    let y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    // 整体背景：输入框底色
    let input_bg = Color::Rgb(65, 69, 76);
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(input_bg)),
        popup_area,
    );

    // 边框色
    let border_clr = Color::Rgb(90, 102, 118);
    let sel_bg = Color::Rgb(255, 204, 163);
    let idle_fg = Color::Rgb(165, 172, 182);

    // ┃ + 2spaces + 内容 + 2spaces + ┃
    let content_width = popup_width.saturating_sub(4) as usize;

    let lines: Vec<Line> = state
        .autocomplete
        .filtered
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let is_sel = i == state.autocomplete.selected;
            let row_bg = if is_sel { sel_bg } else { input_bg };
            let row_fg = idle_fg;

            let text = format!("/{}  {}", cmd.name, cmd.description);
            let text_w = UnicodeWidthStr::width(&text[..]);
            let pad = content_width.saturating_sub(text_w);

            // 边框 ┃ 不设 bg，透出 input_bg；内容区用 row_bg（选中时高亮）
            let content_style = Style::default().fg(row_fg).bg(row_bg);
            let border_style = Style::default().fg(border_clr);

            let mut spans = vec![
                Span::styled("\u{2503}", border_style),
                Span::styled(text, content_style),
            ];
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), content_style));
            }
            spans.push(Span::styled("  ", content_style));
            spans.push(Span::styled("\u{2503}", border_style));

            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(ratatui::text::Text::from(lines)), popup_area);
}

// ===========================================================================
// 交互选择页
// ===========================================================================

fn render_interaction(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    // 弹窗大小：按终端百分比，但有上限
    let pct_w = (area.width as f32 * 0.6) as u16;
    let pct_h = (area.height as f32 * 0.55) as u16;
    let popup_width = pct_w.clamp(40, 80);
    let popup_height = pct_h.clamp(12, 28);
    let popup_area = Rect {
        x: area.x + (area.width - popup_width) / 2,
        y: area.y + (area.height - popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    // 弹窗背景
    let bg = Color::Rgb(35, 38, 50);
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(bg)),
        popup_area,
    );

    let is_thinking = matches!(
        &state.interaction_step,
        Some(InteractionStep::ThinkingEffort { .. })
    );

    // 内部分栏
    let (list_area, hint_area) = if is_thinking {
        // 思考程度：垂直居中（4 行选项）
        let chunks = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .split(popup_area);
        (chunks[1], chunks[3])
    } else {
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(popup_area);
        (chunks[0], chunks[1])
    };

    let items: Vec<ListItem> = match &state.interaction_step {
        Some(InteractionStep::ThinkingEffort { .. }) => Vec::new(),
        Some(InteractionStep::ModelSelection { entries, selected }) => {
            entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    match entry {
                        ModelSelectionEntry::ProviderHeader { name } => {
                            // Provider 标题行：亮色加粗
                            let header_style = Style::default()
                                .fg(Color::Rgb(255, 180, 80))
                                .add_modifier(Modifier::BOLD);
                            ListItem::new(name.clone()).style(header_style)
                        }
                        ModelSelectionEntry::Model { model, .. } => {
                            let is_sel = i == *selected;
                            let style = if is_sel {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(Color::Rgb(100, 180, 255))
                            } else {
                                Style::default().fg(Color::Rgb(200, 200, 210))
                            };
                            let display = model.name.as_deref().unwrap_or(&model.id);
                            ListItem::new(display.to_string()).style(style)
                        }
                    }
                })
                .collect()
        }
        Some(InteractionStep::Session {
            sessions, selected, ..
        }) => sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let prefix = if i == *selected { "▸ " } else { "  " };
                let style = if i == *selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(100, 180, 255))
                } else {
                    Style::default()
                };
                let title_display = if s.title.is_empty() {
                    &s.id[..8]
                } else {
                    &s.title
                };
                let short_id = &s.id[..8];
                ListItem::new(format!(
                    "{}{}  ({} msgs, {})",
                    prefix, title_display, s.message_count, short_id
                ))
                .style(style)
            })
            .collect(),
        None => Vec::new(),
    };

    if let Some(InteractionStep::ThinkingEffort { selected, .. }) = &state.interaction_step {
        let options = ["None", "Low", "Medium", "High"];
        let labels = ["不使用思考", "轻度思考", "适度思考", "深度思考"];
        let label_max = options.iter().map(|s| s.len()).max().unwrap_or(6);
        let lines: Vec<Line> = options
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let style = if i == *selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(100, 180, 255))
                } else {
                    Style::default().fg(Color::Rgb(200, 200, 210))
                };
                let padded = format!("{:<width$}", label, width = label_max);
                Line::from(Span::styled(
                    format!(" {}  ·  {} ", padded, labels[i]),
                    style,
                ))
            })
            .collect();
        // 计算最长行的显示宽度
        let content_width = options
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let padded = format!("{:<width$}", label, width = label_max);
                unicode_width::UnicodeWidthStr::width(
                    format!(" {}  ·  {} ", padded, labels[i]).as_str(),
                )
            })
            .max()
            .unwrap_or(20) as u16;
        // 水平居中内容
        let h_chunks = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(content_width),
            Constraint::Fill(1),
        ])
        .split(list_area);
        let content_area = h_chunks[1];
        let paragraph = Paragraph::new(Text::from(lines)).style(Style::default().bg(bg));
        frame.render_widget(paragraph, content_area);
    } else {
        // 列表（无边框）
        let list = List::new(items).style(Style::default().bg(bg));
        frame.render_widget(list, list_area);
    }

    // 帮助提示（在弹窗底部内部）
    let hint_text = match state.interaction_step {
        Some(InteractionStep::ModelSelection { .. }) => {
            " 选择模型  ·  ↑↓ 选择  Enter 确认  Esc 取消 "
        }
        Some(InteractionStep::ThinkingEffort { .. }) => {
            " 思考程度  ·  ↑↓ 选择  Enter 确认  Esc 返回 "
        }
        Some(InteractionStep::Session { .. }) => " ↑↓ 选择  Enter 确认  Esc 取消 ",
        None => "",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint_text,
            Style::default().fg(Color::Rgb(140, 145, 155)),
        )))
        .style(Style::default().bg(bg)),
        hint_area,
    );
}

// ===========================================================================
// 会话列表（全屏模式）
// ===========================================================================

fn render_session_list(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let Some(InteractionStep::Session {
        sessions,
        selected,
        search,
        ..
    }) = &state.interaction_step
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
    let header_style = Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6));
    let header_lines: Vec<Line> = vec![
        Line::from(Span::styled("  Sessions", header_style)),
        if search.is_empty() {
            Line::from(Span::styled(
                "  Type to Search",
                Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
            ))
        } else {
            Line::from(Span::styled(
                format!("  Search: {}", search),
                Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6)),
            ))
        },
    ];
    frame.render_widget(Paragraph::new(header_lines), header_area);

    // ── Content ──
    let mut lines: Vec<Line> = Vec::with_capacity(content_h);

    if total == 0 {
        // Empty state
        lines.push(Line::from(Span::styled(
            format!("{:width$}", "  No sessions found", width = content_w),
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        )));
        while lines.len() < content_h {
            lines.push(Line::from(Span::styled(
                " ".repeat(content_w),
                Style::default(),
            )));
        }
        frame.render_widget(Paragraph::new(lines), content_area);

        // Divider (empty)
        let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(content_w),
                divider_style,
            ))),
            divider_area,
        );

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Esc back · Type to search",
                Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
            ))),
            footer_area,
        );
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
    let time_width = "2026-05-10 14:30".len() + 2; // time + separator "  "
    let prefix_w = 2; // "❯ " or "  "
    let max_msg_w = content_w.saturating_sub(prefix_w + time_width);

    // ── Build lines ──
    // Top indicator
    if show_top {
        lines.push(Line::from(Span::styled(
            format!("{:width$}", "  ↑ more", width = content_w),
            Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
    }

    // Session items
    for i in 0..max_visible {
        let actual_idx = scroll_off + i;
        let session = &sessions[actual_idx];
        let is_selected = actual_idx == *selected;

        let bg = if is_selected {
            Color::Rgb(0x41, 0x45, 0x4c)
        } else if actual_idx.is_multiple_of(2) {
            Color::Rgb(0x33, 0x37, 0x3f)
        } else {
            Color::Reset
        };

        let fg = if is_selected {
            Color::Rgb(0xc1, 0x97, 0x72)
        } else {
            Color::Rgb(0xa5, 0xac, 0xb6)
        };

        let prefix = if is_selected { "❯ " } else { "  " };
        let time_str = relative_time(session.created_at);
        let msg = truncate_str(&session.first_message, max_msg_w);
        let line_content = format!("{}{}  {}", prefix, time_str, msg);
        let padded = format!("{:<width$}", line_content, width = content_w);

        lines.push(Line::from(Span::styled(
            padded,
            Style::default().fg(fg).bg(bg),
        )));
    }

    // Fill remaining item lines
    while lines.len() < content_h - 1 {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
    }

    // Bottom indicator
    if show_bot {
        lines.push(Line::from(Span::styled(
            format!("{:width$}", "  ↓ more", width = content_w),
            Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
    }

    frame.render_widget(Paragraph::new(lines), content_area);

    // ── Divider ──
    let current = *selected + 1;
    let indicator = format!(" {}/{} ", current, total);
    let dashes_count = content_w.saturating_sub(indicator.len());
    let divider_line = format!("{}{}", "─".repeat(dashes_count), indicator);
    let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(divider_line, divider_style))),
        divider_area,
    );

    // ── Footer ──
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  ↑/↓ navigate · Enter select · Esc back · Type to filter",
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        ))),
        footer_area,
    );
}

/// 将 UTC 时间格式化为相对时间（如 "3m ago", "2h ago", "5d ago"）。
fn relative_time(utc: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(utc);
    let seconds = duration.num_seconds().max(0);
    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 604800 {
        format!("{}d ago", seconds / 86400)
    } else if seconds < 2592000 {
        format!("{}w ago", seconds / 604800)
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

// ===========================================================================
// Header, Messages, Input, Footer (原逻辑不变)
// ===========================================================================

fn render_header(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" omini ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("v0.1.0", Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6))),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if let Some(title) = &state.current_session_title {
                title.as_str()
            } else {
                "No session"
            },
            Style::default().fg(Color::Rgb(0xf6, 0xe2, 0xb7)),
        ),
    ]);

    let paragraph = Paragraph::new(line).style(Style::default().fg(Color::White));
    frame.render_widget(paragraph, area);
}

fn render_messages(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if state.messages.is_empty() && state.pending_assistant.is_none() {
        return;
    }

    let content_width = area.width as usize;
    let visible_height = area.height as usize;
    state.block_ranges.clear();

    let mut all_lines: Vec<Line> = Vec::new();

    let mut tool_result_map: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (mi, msg) in state.messages.iter().enumerate() {
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

    for (msg_idx, message) in state.messages.iter().enumerate() {
        for (block_idx, block) in message.content.iter().enumerate() {
            if let ContentBlock::ToolResult(_) = block
                && consumed.contains(&(msg_idx, block_idx))
            {
                continue;
            }

            let mut block_lines: Vec<Line> = Vec::new();
            let mut block_tool_id: Option<String> = None;

            match block {
                ContentBlock::Text(tb) if message.role == crate::types::message::Role::User => {
                    let user_bg = Color::Rgb(65, 69, 76);
                    let bg_style = Style::default().bg(user_bg);
                    block_lines.push(Line::from(Span::styled(
                        " ".repeat(content_width),
                        bg_style,
                    )));

                    let wrapped =
                        crate::tui::widgets::word_wrap(&tb.text, content_width.saturating_sub(2));
                    if wrapped.is_empty() {
                        let text = format!("❯ {}", " ".repeat(content_width.saturating_sub(2)));
                        block_lines.push(Line::from(Span::styled(text, bg_style)));
                    } else {
                        for (idx, wl) in wrapped.iter().enumerate() {
                            let prefix = if idx == 0 { "❯ " } else { "  " };
                            let text = format!("{}{}", prefix, wl);
                            let text_width = UnicodeWidthStr::width(&*text);
                            let remaining = content_width.saturating_sub(text_width);
                            let full_line = format!("{}{}", text, " ".repeat(remaining));
                            block_lines.push(Line::from(Span::styled(full_line, bg_style)));
                        }
                    }

                    block_lines.push(Line::from(Span::styled(
                        " ".repeat(content_width),
                        bg_style,
                    )));
                }
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    if let Some(positions) = tool_result_map.get(&tu.id) {
                        let tool_result = positions.first().and_then(|(mi, bi)| {
                            if let ContentBlock::ToolResult(tr) = &state.messages[*mi].content[*bi]
                            {
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });

                        let collapsed = !state.expanded_tools.contains(&tu.id);
                        let tool_lines =
                            render_tool(tu, tool_result.as_ref(), content_width, collapsed);
                        block_lines.extend(tool_lines);

                        block_tool_id = Some(tu.id.clone());

                        for pos in positions {
                            consumed.insert(*pos);
                        }
                    } else {
                        // 工具结果尚未返回
                        let tool_lines = render_tool(tu, None, content_width, false);
                        block_lines.extend(tool_lines);
                        block_tool_id = Some(tu.id.clone());
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
                }
                let base = all_lines.len();
                let block_len = block_lines.len();
                if let Some(tool_id) = block_tool_id.take() {
                    state.block_ranges.push((base..base + block_len, tool_id));
                }
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

    let paragraph =
        Paragraph::new(ratatui::text::Text::from(all_lines)).scroll((scroll_y as u16, 0));

    frame.render_widget(paragraph, area);
}

fn render_input(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let bg = Style::default().bg(Color::Rgb(65, 69, 76));

    let bg_widget = Paragraph::new(Line::from("")).style(bg);
    frame.render_widget(bg_widget, area);

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let input_line = chunks[1];

    let line_bg = Paragraph::new(Line::from(Span::styled(
        " ".repeat(area.width as usize),
        bg,
    )))
    .style(bg);
    frame.render_widget(line_bg, input_line);

    let content = if state.input.is_empty() {
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(Color::Cyan)),
            Span::styled("Type a message...", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(Color::Cyan)),
            Span::raw(&state.input),
        ])
    };
    let paragraph = Paragraph::new(content);
    frame.render_widget(paragraph, input_line);

    let cursor_x = if state.input.is_empty() {
        input_line.x + 2
    } else {
        let byte_idx = state.char_to_byte(state.cursor_char);
        let prefix_width = UnicodeWidthStr::width(&state.input[..byte_idx]);
        input_line.x + 2 + prefix_width as u16
    };
    frame.set_cursor_position((cursor_x, input_line.y));
}

fn render_footer(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    use crate::types::config::ThinkingEffort;

    let path_display = {
        let cwd_str = state.status_bar.cwd.to_string_lossy();
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() && cwd_str.starts_with(&home) {
            format!("~{}", &cwd_str[home.len()..])
        } else {
            cwd_str.to_string()
        }
    };

    let model_part = if state.status_bar.active_provider.is_empty() {
        state.status_bar.model.clone()
    } else {
        format!(
            "{}/{}",
            state.status_bar.active_provider, state.status_bar.model
        )
    };

    let model_thinking = match state.status_bar.thinking_effort {
        Some(ThinkingEffort::Low) => format!("{} low", model_part),
        Some(ThinkingEffort::Medium) => format!("{} medium", model_part),
        Some(ThinkingEffort::High) => format!("{} high", model_part),
        _ => model_part,
    };

    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!(" {} ", model_thinking),
            Style::default().fg(Color::Rgb(0xf6, 0xe2, 0xb7)),
        ),
        Span::styled("·", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {} ", path_display),
            Style::default().fg(Color::Rgb(0xab, 0xdf, 0xa7)),
        ),
        Span::styled("·", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {} ", state.agent_status),
            Style::default().fg(Color::Rgb(0xc8, 0xa9, 0xee)),
        ),
    ]);

    let paragraph = Paragraph::new(line).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}
