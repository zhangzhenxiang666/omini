use super::{
    INPUT_BG, apply_text_selection_highlight, build_assistant_text_lines, build_llm_summary_lines,
    build_proposed_plan_lines, line_to_plain_text, line_width, render_subagent_tool,
    styled_wrapped_display, styled_wrapped_text, truncate_str,
};
use crate::state::{UiMessage, UiState, format_run_duration};
use crate::types::display::DisplayMessage;
use crate::types::message::ContentBlock;
use crate::widgets::{
    build_bordered_lines, build_thinking_lines, render_tool, tool_error_display_text,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

fn build_display_message_lines(
    display: &DisplayMessage,
    content_width: usize,
) -> Vec<Line<'static>> {
    let user_bg = INPUT_BG;
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

enum AlertKind {
    Notice,
    Warning,
    Error,
}

fn build_alert_lines(text: &str, content_width: usize, kind: AlertKind) -> Vec<Line<'static>> {
    let (label, color) = match kind {
        AlertKind::Notice => ("info", Color::Rgb(0x7a, 0xba, 0xff)),
        AlertKind::Warning => ("warn", Color::Rgb(0xd4, 0xb6, 0x6a)),
        AlertKind::Error => ("error", Color::Rgb(255, 100, 100)),
    };
    let prefix = format!("[{label}] ");
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

fn build_run_divider_line(elapsed: Duration, content_width: usize) -> Vec<Line<'static>> {
    let style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
    let label = format!("─ Worked for {} ", format_run_divider_duration(elapsed));
    let label_width = UnicodeWidthStr::width(label.as_str());
    if content_width <= label_width + 1 {
        return vec![Line::from(Span::styled(
            truncate_str(label.trim(), content_width),
            style,
        ))];
    }

    vec![Line::from(vec![
        Span::styled(label, style),
        Span::styled("─".repeat(content_width - label_width), style),
    ])]
}

fn format_run_divider_duration(duration: Duration) -> String {
    format_run_duration(duration)
        .replace('h', "h ")
        .replace('m', "m ")
        .trim()
        .to_string()
}

pub(super) fn render_messages(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if state.messages.is_empty()
        && state.pending_assistant.is_none()
        && state.pending_proposed_plan.is_none()
    {
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
        if let UiMessage::RunDivider { elapsed } = ui_message {
            let block_lines = build_run_divider_line(*elapsed, content_width);
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

        if let UiMessage::ProposedPlan { text } = ui_message {
            let block_lines = build_proposed_plan_lines(text, content_width);
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

        if let UiMessage::CompactSummary { text } = ui_message {
            let block_lines = build_llm_summary_lines(text, content_width);
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
                UiMessage::RunDivider { .. } => unreachable!(),
                UiMessage::Display(_) => unreachable!(),
                UiMessage::ProposedPlan { .. } => unreachable!(),
                UiMessage::CompactSummary { .. } => unreachable!(),
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
                    let user_bg = INPUT_BG;
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
                    let mut lines = build_assistant_text_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::Image(_) => {}
                ContentBlock::ToolUse(tu) => {
                    let tool_pause = state.tool_pause_for_tool_use(&tu.id);
                    let tool_pause_active =
                        tool_pause.map(|pause| state.is_active_tool_pause(pause));
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
                            &state.pending_tool_pauses,
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
                            tool_pause,
                            tool_pause_active,
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
                            tool_pause,
                            tool_pause_active,
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
                    let content = if tr.is_error {
                        tool_error_display_text(&tr.content)
                    } else {
                        tr.content.clone()
                    };
                    let mut lines =
                        build_bordered_lines(&content, content_width, color, false, None);
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
                    let mut lines = build_assistant_text_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::Image(_) => {}
                ContentBlock::Thinking(tb) => {
                    let mut lines = build_thinking_lines(&tb.thinking, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    let tool_pause = state.tool_pause_for_tool_use(&tu.id);
                    let tool_pause_active =
                        tool_pause.map(|pause| state.is_active_tool_pause(pause));
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
                            &state.pending_tool_pauses,
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
                            tool_pause,
                            tool_pause_active,
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
                    let content = if tr.is_error {
                        tool_error_display_text(&tr.content)
                    } else {
                        tr.content.clone()
                    };
                    let mut lines =
                        build_bordered_lines(&content, content_width, color, false, None);
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

    if let Some(plan) = &state.pending_proposed_plan
        && !plan.trim().is_empty()
    {
        let block_lines = build_proposed_plan_lines(plan, content_width);
        if !block_lines.is_empty() {
            if !all_lines.is_empty() {
                all_lines.push(Line::from(""));
                selectable_lines.push(String::new());
            }
            selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
            all_lines.extend(block_lines);
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

    let user_bg = INPUT_BG;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_divider_renders_elapsed_duration() {
        let lines = build_run_divider_line(Duration::from_secs(67), 24);
        let text = line_to_plain_text(&lines[0]);

        assert_eq!(lines.len(), 1);
        assert!(text.starts_with("─ Worked for 1m 07s ─"));
        assert!(UnicodeWidthStr::width(text.as_str()) <= 24);
    }

    #[test]
    fn run_divider_does_not_exceed_narrow_width() {
        let lines = build_run_divider_line(Duration::from_secs(67), 4);
        let text = line_to_plain_text(&lines[0]);

        assert_eq!(lines.len(), 1);
        assert!(UnicodeWidthStr::width(text.as_str()) <= 4);
    }
}
