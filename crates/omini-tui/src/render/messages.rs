use super::{
    INPUT_BG, build_assistant_text_lines, build_llm_summary_lines, build_proposed_plan_lines,
    line_to_plain_text, line_width, render_subagent_tool, styled_wrapped_display,
    styled_wrapped_text, truncate_str,
};
use crate::state::{UiMessage, UiState, format_run_duration};
use crate::types::events::{Notification, NotificationKind};
use crate::widgets::{
    build_bordered_lines, build_thinking_lines, render_tool, tool_error_display_text,
    truncate_display_width,
};
use omini_domain::display::DisplayMessage;
use omini_domain::message::ContentBlock;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
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

fn build_notification_lines(
    notification: &Notification,
    content_width: usize,
) -> Vec<Line<'static>> {
    let color = match notification.kind {
        NotificationKind::Info => Color::Rgb(0x7a, 0xba, 0xff),
        NotificationKind::Warn => Color::Rgb(0xd4, 0xb6, 0x6a),
        NotificationKind::Error => Color::Rgb(255, 100, 100),
    };
    let style = Style::default().fg(color);
    let detail_style = Style::default().fg(Color::Rgb(140, 142, 150));
    let mut lines = Vec::new();

    let prefix = "· ";
    let prefix_width = UnicodeWidthStr::width(prefix);
    if content_width <= prefix_width {
        lines.push(Line::from(Span::styled(
            truncate_display_width(prefix.trim_end(), content_width),
            style,
        )));
    } else {
        let wrap_width = content_width.saturating_sub(prefix_width).max(1);
        let wrapped = crate::widgets::word_wrap(&notification.message, wrap_width);
        if wrapped.is_empty() {
            lines.push(Line::from(Span::styled(prefix, style)));
        } else {
            let continuation = " ".repeat(prefix_width);
            for (idx, line) in wrapped.into_iter().enumerate() {
                let current_prefix = if idx == 0 {
                    prefix.to_string()
                } else {
                    continuation.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled(current_prefix, style),
                    Span::styled(line, style),
                ]));
            }
        }
    }

    let mut first_detail = true;
    for detail in notification
        .details
        .iter()
        .map(|detail| detail.trim())
        .filter(|detail| !detail.is_empty())
    {
        let detail_prefix = if first_detail { "  └ " } else { "    " };
        first_detail = false;
        let detail_width = content_width.saturating_sub(UnicodeWidthStr::width(detail_prefix));
        lines.push(Line::from(vec![
            Span::styled(detail_prefix, detail_style),
            Span::styled(truncate_display_width(detail, detail_width), detail_style),
        ]));
    }

    lines
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
        && state.pending_compact_summary.is_none()
    {
        state.selectable_message_lines.clear();
        state.message_scroll_y = 0;
        return;
    }

    let content_width = area.width as usize;
    let visible_height = area.height as usize;

    // 1. completed 段：缓存仅覆盖 live_message_start 之前的消息。
    //    含未完成工具（pending tool use）的消息不进入缓存，而是作为 live 段每帧重渲染。

    let live_start = state.live_message_start.min(state.messages.len());
    let dims_match = state.render_cache.completed_content_width == content_width
        && state.render_cache.completed_show_thinking == state.show_thinking_blocks;

    if state.render_cache.completed_message_count == 0 || !dims_match {
        // 缓存完全失效或维度变化 → 全量重建到 live 边界
        let (lines, sel) = render_message_range(state, content_width, 0, Some(live_start));
        state.render_cache.completed_lines = lines;
        state.render_cache.completed_selectable = sel;
        state.render_cache.completed_message_count = live_start;
        state.render_cache.completed_content_width = content_width;
        state.render_cache.completed_show_thinking = state.show_thinking_blocks;
    } else if state.render_cache.completed_message_count < live_start {
        // 增量追加到 live 边界
        let start_idx = state.render_cache.completed_message_count;
        let (new_lines, new_sel) =
            render_message_range(state, content_width, start_idx, Some(live_start));
        if !new_lines.is_empty() && !state.render_cache.completed_lines.is_empty() {
            state.render_cache.completed_lines.push(Line::from(""));
            state.render_cache.completed_selectable.push(String::new());
        }
        state.render_cache.completed_lines.extend(new_lines);
        state.render_cache.completed_selectable.extend(new_sel);
        state.render_cache.completed_message_count = live_start;
    }
    // else: 缓存命中到 live 边界，无需操作

    // live 段：含运行中 subagent 的消息，每帧重渲染（呼吸灯动画 + 子工具实时更新）
    let live_lines = if live_start < state.messages.len() {
        render_message_range(state, content_width, live_start, None)
    } else {
        (Vec::new(), Vec::new())
    };

    // 2. pending / plan 段：每帧直接重算（量小，不值得缓存）

    let pending_lines = state
        .pending_assistant
        .as_ref()
        .map(|_| render_pending_assistant_lines(state, content_width))
        .unwrap_or_default();

    let plan_lines = state
        .pending_proposed_plan
        .as_ref()
        .filter(|plan| !plan.trim().is_empty())
        .map(|plan| render_pending_plan_lines(plan, content_width))
        .unwrap_or_default();

    let compact_lines = state
        .pending_compact_summary
        .as_ref()
        .map(|text| render_pending_compact_lines(text, content_width))
        .unwrap_or_default();

    // 3. 计算分段布局
    // 布局顺序：completed(缓存) → live(每帧) → pending → plan → compact

    let n_completed = state.render_cache.completed_lines.len();
    let n_live = live_lines.0.len();
    let n_pending = pending_lines.0.len();
    let n_plan = plan_lines.0.len();
    let n_compact = compact_lines.0.len();
    let has_sep1 = n_live > 0 && n_completed > 0;
    let has_sep2 = n_pending > 0 && (n_completed > 0 || n_live > 0);
    let has_sep3 = n_plan > 0 && (n_completed > 0 || n_live > 0 || n_pending > 0);
    let has_sep4 = n_compact > 0 && (n_completed > 0 || n_live > 0 || n_pending > 0 || n_plan > 0);
    let live_offset = n_completed + has_sep1 as usize;
    let pending_offset = live_offset + n_live + has_sep2 as usize;
    let plan_offset = pending_offset + n_pending + has_sep3 as usize;
    let compact_offset = plan_offset + n_plan + has_sep4 as usize;
    let total_lines = compact_offset + n_compact;

    // 4. 滚动计算

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
    state.message_scroll_y = scroll_y;

    // 5. 构建 selectable_message_lines

    state.selectable_message_lines.clear();
    state
        .selectable_message_lines
        .extend_from_slice(&state.render_cache.completed_selectable);
    if has_sep1 {
        state.selectable_message_lines.push(String::new());
    }
    state
        .selectable_message_lines
        .extend_from_slice(&live_lines.1);
    if has_sep2 {
        state.selectable_message_lines.push(String::new());
    }
    state
        .selectable_message_lines
        .extend_from_slice(&pending_lines.1);
    if has_sep3 {
        state.selectable_message_lines.push(String::new());
    }
    state
        .selectable_message_lines
        .extend_from_slice(&plan_lines.1);
    if has_sep4 {
        state.selectable_message_lines.push(String::new());
    }
    state
        .selectable_message_lines
        .extend_from_slice(&compact_lines.1);

    // 6. 注册可选中文本行（用于鼠标拖选反查）

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

    // 7. 逐段直接渲染到 buffer（零拷贝）

    let render_ctx = SectionRenderContext {
        scroll_y,
        visible_height,
        area,
        user_bg: INPUT_BG,
        user_line_bg: Style::default().bg(INPUT_BG),
    };

    let buf = frame.buffer_mut();

    render_cached_section(&state.render_cache.completed_lines, 0, &render_ctx, buf);

    if has_sep1 {
        render_blank_separator(n_completed, scroll_y, visible_height, area, buf);
    }

    render_cached_section(&live_lines.0, live_offset, &render_ctx, buf);

    if has_sep2 {
        render_blank_separator(live_offset + n_live, scroll_y, visible_height, area, buf);
    }

    render_cached_section(&pending_lines.0, pending_offset, &render_ctx, buf);

    if has_sep3 {
        render_blank_separator(
            pending_offset + n_pending,
            scroll_y,
            visible_height,
            area,
            buf,
        );
    }

    render_cached_section(&plan_lines.0, plan_offset, &render_ctx, buf);

    if has_sep4 {
        render_blank_separator(plan_offset + n_plan, scroll_y, visible_height, area, buf);
    }

    render_cached_section(&compact_lines.0, compact_offset, &render_ctx, buf);
}

/// 渲染缓存段所需的上下文参数。
struct SectionRenderContext {
    scroll_y: usize,
    visible_height: usize,
    area: Rect,
    user_bg: Color,
    user_line_bg: Style,
}

/// 将一个缓存段直接渲染到 buffer，不拷贝 `Line`。
fn render_cached_section(
    lines: &[Line<'static>],
    section_start: usize,
    ctx: &SectionRenderContext,
    buf: &mut ratatui::buffer::Buffer,
) {
    let skip = ctx.scroll_y.saturating_sub(section_start);
    for (local_idx, line) in lines.iter().enumerate().skip(skip) {
        let visible_row = section_start + local_idx - ctx.scroll_y;
        if visible_row >= ctx.visible_height {
            break;
        }
        let row_area = Rect::new(
            ctx.area.x,
            ctx.area.y + visible_row as u16,
            ctx.area.width,
            1,
        );

        line.render(row_area, buf);

        if line.style.bg == Some(ctx.user_bg) {
            buf.set_style(row_area, ctx.user_line_bg);
        }
    }
}

/// 渲染分段之间的空行分隔符。
fn render_blank_separator(
    abs_idx: usize,
    scroll_y: usize,
    visible_height: usize,
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    if abs_idx < scroll_y {
        return;
    }
    let visible_row = abs_idx - scroll_y;
    if visible_row >= visible_height {
        return;
    }
    let row_area = Rect::new(area.x, area.y + visible_row as u16, area.width, 1);
    Line::from("").render(row_area, buf);
}

/// 渲染 `messages[start_idx..end_idx]`，返回该范围的渲染结果（不含前导分隔符）。
///
/// `end_idx` 为 `None` 时渲染到末尾；为 `Some(n)` 时只渲染到 `messages[n]`（不含）。
/// 用于增量追加缓存：当只有新消息追加时，只渲染 `start_idx` 之后的部分，
/// 然后将结果追加到已有缓存。
fn render_message_range(
    state: &UiState,
    content_width: usize,
    start_idx: usize,
    end_idx: Option<usize>,
) -> (Vec<Line<'static>>, Vec<String>) {
    let end_idx = end_idx.unwrap_or(state.messages.len());
    let mut all_lines: Vec<Line> = Vec::new();
    let mut selectable_lines: Vec<String> = Vec::new();

    // 全量扫描构建 tool_result_map（成本低，O(n)）
    let rendered_messages: Vec<&omini_domain::message::Message> = state
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

    // 计算 rendered_msg_offset：messages[..start_idx] 中 UiMessage::Message 变体的数量
    let rendered_msg_offset = state.messages[..start_idx]
        .iter()
        .filter(|m| matches!(m, UiMessage::Message(_)))
        .count();

    // 预填充 consumed：扫描 messages[..start_idx] 中的 ToolUse block，
    // 将其对应的 ToolResult 位置加入 consumed（避免跨消息引用导致重复渲染）
    let mut pre_rendered_idx = 0;
    for ui_message in &state.messages[..start_idx] {
        if let UiMessage::Message(message) = ui_message {
            let msg_idx = pre_rendered_idx;
            pre_rendered_idx += 1;
            for (block_idx, block) in message.content.iter().enumerate() {
                if let ContentBlock::ToolUse(tu) = block
                    && let Some(positions) = tool_result_map.get(&tu.id)
                {
                    for pos in positions {
                        consumed.insert(*pos);
                    }
                }
                // 同时标记本消息内已被引用的 ToolResult
                if let ContentBlock::ToolResult(_) = block {
                    consumed.insert((msg_idx, block_idx));
                }
            }
        }
    }

    // 渲染 messages[start_idx..end_idx]
    let mut rendered_msg_idx = rendered_msg_offset;
    for ui_message in &state.messages[start_idx..end_idx] {
        let (msg_lines, msg_sel) = render_single_ui_message(
            ui_message,
            &mut rendered_msg_idx,
            &rendered_messages,
            &tool_result_map,
            &mut consumed,
            state,
            content_width,
        );
        if !msg_lines.is_empty() {
            if !all_lines.is_empty() {
                all_lines.push(Line::from(""));
                selectable_lines.push(String::new());
            }
            all_lines.extend(msg_lines);
            selectable_lines.extend(msg_sel);
        }
    }

    (all_lines, selectable_lines)
}

/// 渲染单条 `UiMessage`，返回 `(lines, selectable)`。
///
/// 不负责消息间分隔符（由 `render_message_range` 负责），
/// 只返回该消息自身产出的行（内部 block 间有分隔空行）。
fn render_single_ui_message(
    ui_message: &UiMessage,
    rendered_msg_idx: &mut usize,
    rendered_messages: &[&omini_domain::message::Message],
    tool_result_map: &HashMap<String, Vec<(usize, usize)>>,
    consumed: &mut HashSet<(usize, usize)>,
    state: &UiState,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let mut all_lines: Vec<Line> = Vec::new();
    let mut selectable_lines: Vec<String> = Vec::new();

    match ui_message {
        UiMessage::RunDivider { elapsed } => {
            let block_lines = build_run_divider_line(*elapsed, content_width);
            selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
            all_lines.extend(block_lines);
        }
        UiMessage::Display(display) => {
            let block_lines = build_display_message_lines(display, content_width);
            selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
            all_lines.extend(block_lines);
        }
        UiMessage::ProposedPlan { text } => {
            let block_lines = build_proposed_plan_lines(text, content_width);
            selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
            all_lines.extend(block_lines);
        }
        UiMessage::CompactSummary { text } => {
            let block_lines = build_llm_summary_lines(text, content_width);
            selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
            all_lines.extend(block_lines);
        }
        UiMessage::Notification(notification) => {
            let block_lines = build_notification_lines(notification, content_width);
            selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
            all_lines.extend(block_lines);
        }
        UiMessage::Message(message) => {
            let msg_idx = *rendered_msg_idx;
            *rendered_msg_idx += 1;

            for (block_idx, block) in message.content.iter().enumerate() {
                if let ContentBlock::ToolResult(_) = block
                    && consumed.contains(&(msg_idx, block_idx))
                {
                    continue;
                }

                let mut block_lines: Vec<Line> = Vec::new();
                match block {
                    ContentBlock::Text(tb) if message.role == omini_domain::message::Role::User => {
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
                            block_lines
                                .push(Line::from(Span::styled(text, bg_style)).style(bg_style));
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
                        if !state.show_thinking_blocks {
                            continue;
                        }
                        let mut lines = build_thinking_lines(&tb.thinking, content_width);
                        block_lines.append(&mut lines);
                    }
                    ContentBlock::ToolResult(tr) => {
                        let color = if tr.is_error {
                            Color::Rgb(255, 100, 100)
                        } else {
                            Color::Rgb(100, 200, 130)
                        };
                        let error_content;
                        let content_ref = if tr.is_error {
                            error_content = tool_error_display_text(&tr.content);
                            &error_content
                        } else {
                            &tr.content
                        };
                        let mut lines =
                            build_bordered_lines(content_ref, content_width, color, false, None);
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
    }

    (all_lines, selectable_lines)
}

/// 渲染 pending_assistant（流式增量内容）。
fn render_pending_assistant_lines(
    state: &UiState,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let mut all_lines: Vec<Line> = Vec::new();
    let mut selectable_lines: Vec<String> = Vec::new();

    let Some(pending) = &state.pending_assistant else {
        return (all_lines, selectable_lines);
    };

    // 先构建 pending_assistant 内部的 tool_result_map
    let mut tr_indices: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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
                if !state.show_thinking_blocks {
                    continue;
                }
                let mut lines = build_thinking_lines(&tb.thinking, content_width);
                block_lines.append(&mut lines);
            }
            ContentBlock::ToolUse(tu) => {
                let tool_pause = state.tool_pause_for_tool_use(&tu.id);
                let tool_pause_active = tool_pause.map(|pause| state.is_active_tool_pause(pause));
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
                let error_content;
                let content_ref = if tr.is_error {
                    error_content = tool_error_display_text(&tr.content);
                    &error_content
                } else {
                    &tr.content
                };
                let mut lines =
                    build_bordered_lines(content_ref, content_width, color, false, None);
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

    (all_lines, selectable_lines)
}

/// 渲染 pending_proposed_plan。
fn render_pending_plan_lines(
    plan_text: &str,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let block_lines = build_proposed_plan_lines(plan_text, content_width);
    let mut all_lines = Vec::new();
    let mut selectable_lines = Vec::new();
    if !block_lines.is_empty() {
        selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
        all_lines.extend(block_lines);
    }
    (all_lines, selectable_lines)
}

/// 渲染正在流式构建中的 compact 摘要（不走缓存，每帧重算以驱动呼吸动画）。
fn render_pending_compact_lines(
    summary_text: &str,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let block_lines = build_llm_summary_lines(summary_text, content_width);
    let mut all_lines = Vec::new();
    let mut selectable_lines = Vec::new();
    if !block_lines.is_empty() {
        selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
        all_lines.extend(block_lines);
    }
    (all_lines, selectable_lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_domain::message::{ContentBlock, Message, Role};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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

    #[test]
    fn notification_details_use_single_connector_and_truncate_by_display_width() {
        let notification = Notification::warning("主消息").with_details(vec![
            "ok".to_string(),
            "中文abcdef".to_string(),
            "   ".to_string(),
            "done".to_string(),
        ]);

        let lines = build_notification_lines(&notification, 14);
        let plain = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert_eq!(
            plain,
            vec!["· 主消息", "  └ ok", "    中文abcdef", "    done"]
        );
        assert!(
            plain
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= 14)
        );
    }

    #[test]
    fn notification_kind_sets_main_line_color() {
        let lines = build_notification_lines(&Notification::error("failed"), 80);

        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(255, 100, 100)));
    }

    #[test]
    fn thinking_blocks_render_when_enabled() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.messages.push(UiMessage::Message(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::from_thinking("checking context".to_string()),
                ContentBlock::from_text("done".to_string()),
            ],
        )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 12)))
            .unwrap();

        assert!(
            state
                .selectable_message_lines
                .iter()
                .any(|line| line.contains("Thinking: checking context"))
        );
    }

    #[test]
    fn thinking_blocks_are_hidden_when_disabled() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.show_thinking_blocks = false;
        state.messages.push(UiMessage::Message(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::from_thinking("checking context".to_string()),
                ContentBlock::from_text("done".to_string()),
            ],
        )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 12)))
            .unwrap();

        assert!(
            !state
                .selectable_message_lines
                .iter()
                .any(|line| line.contains("Thinking: checking context"))
        );
        assert!(
            state
                .selectable_message_lines
                .iter()
                .any(|line| line.contains("done"))
        );
    }
}
