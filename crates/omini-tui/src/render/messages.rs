use super::activity::{
    activity_group_end, render_activity_summary, render_pending_activity_group, tool_result_index,
    trailing_activity_group_start,
};
use super::{
    INPUT_BG, build_assistant_text_lines, build_llm_summary_lines, build_proposed_plan_lines,
    line_to_plain_text, line_width, render_subagent_tool, styled_wrapped_display,
    styled_wrapped_text, truncate_str,
};
use crate::state::{UiMessage, UiState, format_run_duration};
use crate::types::events::{Notification, NotificationKind};
use crate::widgets::{
    build_bordered_lines, format_thinking_duration, is_special_tool, render_read_task, render_tool,
    render_tool_compact, render_wait_agents, thinking_duration_line, tool_error_display_text,
    truncate_display_width,
};
use omini_domain::display::DisplayMessage;
use omini_domain::message::{ContentBlock, ToolUseBlock};
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
    let mut lines = Vec::new();

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

    let prefix = "⏺ ";
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

    let active_group_start = trailing_activity_group_start(&state.messages);
    let live_start = state
        .live_message_start
        .min(active_group_start.unwrap_or(usize::MAX))
        .min(state.messages.len());
    let dims_match = state.render_cache.completed_content_width == content_width;

    if state.render_cache.completed_message_count == 0
        || !dims_match
        || state.render_cache.completed_message_count > live_start
    {
        // 缓存完全失效、维度变化或 live 边界回退时，全量重建到 live 边界。
        let (lines, sel) = render_message_range(state, content_width, 0, Some(live_start), None);
        state.render_cache.completed_lines = lines;
        state.render_cache.completed_selectable = sel;
        state.render_cache.completed_message_count = live_start;
        state.render_cache.completed_content_width = content_width;
    } else if state.render_cache.completed_message_count < live_start {
        // 增量追加到 live 边界
        let start_idx = state.render_cache.completed_message_count;
        let (new_lines, new_sel) =
            render_message_range(state, content_width, start_idx, Some(live_start), None);
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
    let pending_activity_prefix_len = state
        .pending_assistant
        .as_ref()
        .map(|pending| activity_prefix_len(state, pending));
    let suppress_open_group = active_group_start
        .filter(|_| pending_activity_prefix_len.is_some_and(|prefix_len| prefix_len > 0));
    let live_lines = if live_start < state.messages.len() {
        render_message_range(state, content_width, live_start, None, suppress_open_group)
    } else {
        (Vec::new(), Vec::new())
    };

    // 2. pending / plan 段：每帧直接重算（量小，不值得缓存）

    let pending_lines = state
        .pending_assistant
        .as_ref()
        .map(|_| {
            let prefix_len = pending_activity_prefix_len.unwrap_or_default();
            if prefix_len > 0 {
                let mut group = render_pending_activity_group(
                    state,
                    suppress_open_group.unwrap_or(state.messages.len()),
                    prefix_len,
                    content_width,
                );
                let pending_body = render_pending_assistant_lines(state, content_width, prefix_len);
                append_message_lines(&mut group.0, &mut group.1, pending_body.0, pending_body.1);
                group
            } else {
                render_pending_assistant_lines(state, content_width, 0)
            }
        })
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

fn activity_prefix_len(state: &UiState, message: &omini_domain::message::Message) -> usize {
    message
        .content
        .iter()
        .take_while(|block| match block {
            ContentBlock::Thinking(_) | ContentBlock::ToolResult(_) | ContentBlock::Image(_) => {
                true
            }
            ContentBlock::ToolUse(tool_use) => {
                !is_special_tool(tool_use) && state.tool_pause_for_tool_use(&tool_use.id).is_none()
            }
            ContentBlock::Text(text) if text.text.trim().is_empty() => true,
            ContentBlock::Text(_) => false,
        })
        .count()
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
    suppress_activity_from: Option<usize>,
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

    let tool_result_map = tool_result_index(&rendered_messages);
    let render_context = MessageRenderContext {
        rendered_messages: &rendered_messages,
        tool_result_map: &tool_result_map,
        state,
        content_width,
    };

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

    // Merge adjacent assistant messages that contain only hidden reasoning and
    // ordinary tool activity. A visible answer or an interaction tool closes the group.
    let mut rendered_msg_idx = rendered_msg_offset;
    let mut ui_idx = start_idx;
    while ui_idx < end_idx {
        if let Some(group_end) = activity_group_end(&state.messages, ui_idx, end_idx) {
            let group = state.messages[ui_idx..group_end]
                .iter()
                .filter_map(UiMessage::as_message)
                .collect::<Vec<_>>();
            let summary = render_activity_summary(
                &group,
                &tool_result_map,
                &mut consumed,
                content_width,
                None,
            );
            if !suppress_activity_from.is_some_and(|from| ui_idx >= from) {
                let summary_selectable = summary.iter().map(line_to_plain_text).collect();
                append_message_lines(
                    &mut all_lines,
                    &mut selectable_lines,
                    summary,
                    summary_selectable,
                );
            }

            for ui_message in &state.messages[ui_idx..group_end] {
                let (msg_lines, msg_sel) = render_single_ui_message(
                    ui_message,
                    &mut rendered_msg_idx,
                    &render_context,
                    &mut consumed,
                    false,
                );
                append_message_lines(&mut all_lines, &mut selectable_lines, msg_lines, msg_sel);
            }
            ui_idx = group_end;
            continue;
        }

        let (msg_lines, msg_sel) = render_single_ui_message(
            &state.messages[ui_idx],
            &mut rendered_msg_idx,
            &render_context,
            &mut consumed,
            true,
        );
        append_message_lines(&mut all_lines, &mut selectable_lines, msg_lines, msg_sel);
        ui_idx += 1;
    }

    (all_lines, selectable_lines)
}

struct MessageRenderContext<'a> {
    rendered_messages: &'a [&'a omini_domain::message::Message],
    tool_result_map: &'a HashMap<String, Vec<(usize, usize)>>,
    state: &'a UiState,
    content_width: usize,
}

fn append_message_lines(
    all_lines: &mut Vec<Line<'static>>,
    selectable_lines: &mut Vec<String>,
    mut lines: Vec<Line<'static>>,
    mut selectable: Vec<String>,
) {
    if lines.is_empty() {
        return;
    }
    if !all_lines.is_empty() {
        all_lines.push(Line::from(""));
        selectable_lines.push(String::new());
    }
    all_lines.append(&mut lines);
    selectable_lines.append(&mut selectable);
}

/// 渲染单条 `UiMessage`，返回 `(lines, selectable)`。
///
/// 不负责消息间分隔符（由 `render_message_range` 负责），
/// 只返回该消息自身产出的行（内部 block 间有分隔空行）。
fn render_single_ui_message(
    ui_message: &UiMessage,
    rendered_msg_idx: &mut usize,
    context: &MessageRenderContext<'_>,
    consumed: &mut HashSet<(usize, usize)>,
    include_activity_summary: bool,
) -> (Vec<Line<'static>>, Vec<String>) {
    let MessageRenderContext {
        rendered_messages,
        tool_result_map,
        state,
        content_width: _,
    } = context;
    let content_width = context.content_width;
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
        UiMessage::AgentTaskNotification(notification) => {
            let dim = Style::default().fg(Color::Rgb(140, 145, 155));
            for task in &notification.tasks {
                let text = format!(
                    "agent task · {} · {} · {}",
                    task.agent,
                    task.title,
                    task.status.as_str()
                );
                let text = truncate_str(&text, content_width);
                selectable_lines.push(text.clone());
                all_lines.push(Line::from(Span::styled(text, dim)));
            }
        }
        UiMessage::Message(message) => {
            let msg_idx = *rendered_msg_idx;
            *rendered_msg_idx += 1;

            // Claude Code 式活动收缩：assistant 消息的 thinking 时长与常规工具
            // 聚合为一行摘要（`⏺ Thought for 12s, ran 1 shell command`），
            // 常规工具按类别计数；特殊交互工具与正文保持独立渲染。
            if include_activity_summary && message.role == omini_domain::message::Role::Assistant {
                let summary_lines = render_activity_summary(
                    &[message],
                    tool_result_map,
                    consumed,
                    content_width,
                    None,
                );
                if !summary_lines.is_empty() {
                    selectable_lines.extend(summary_lines.iter().map(line_to_plain_text));
                    all_lines.extend(summary_lines);
                }
            }

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
                    }
                    ContentBlock::Text(tb) => {
                        let mut lines = build_assistant_text_lines(&tb.text, content_width);
                        block_lines.append(&mut lines);
                    }
                    ContentBlock::Image(_) => {}
                    // thinking 块已并入活动聚合摘要；思考内容文本不再展示
                    ContentBlock::Thinking(_) => continue,
                    ContentBlock::ToolUse(tu) => {
                        // 常规工具已由活动聚合摘要按类别计数。
                        if !is_special_tool(tu) {
                            continue;
                        }
                        let tool_pause = state.tool_pause_for_tool_use(&tu.id);
                        let tool_pause_active =
                            tool_pause.map(|pause| state.is_active_tool_pause(pause));
                        if matches!(tu.name.as_str(), "spawn_agent" | "run_agent") {
                            let node = state
                                .subagents_by_tool_use
                                .get(&tu.id)
                                .and_then(|thread_id| state.subagents.get(thread_id));
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
                        } else if tu.name == "read_task" {
                            block_lines.extend(render_read_task(
                                tu,
                                read_task_label(state, tu),
                                false,
                            ));
                            if let Some(positions) = tool_result_map.get(&tu.id) {
                                for pos in positions {
                                    consumed.insert(*pos);
                                }
                            }
                        } else if tu.name == "wait_agents" {
                            block_lines.extend(render_wait_agents(tu, false));
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
    skip_prefix: usize,
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
        if block_idx < skip_prefix {
            continue;
        }
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
                // 思考内容不再展示；进行中的段显示动态计时，已结算的段显示静态时长
                let label = if let Some(ms) = tb.duration_ms {
                    format!("  Thought for {}", format_thinking_duration(ms))
                } else if let Some(started_at) = state.thinking_started_at {
                    let secs = started_at.elapsed().as_secs();
                    if secs >= 1 {
                        format!("  Thinking for {secs}s...")
                    } else {
                        "  Thinking...".to_string()
                    }
                } else {
                    "  Thinking...".to_string()
                };
                block_lines.push(thinking_duration_line(&label));
            }
            ContentBlock::ToolUse(tu) => {
                if matches!(tu.name.as_str(), "spawn_agent" | "run_agent") {
                    let node = state
                        .subagents_by_tool_use
                        .get(&tu.id)
                        .and_then(|thread_id| state.subagents.get(thread_id));
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
                } else if tu.name == "read_task" {
                    block_lines.extend(render_read_task(tu, read_task_label(state, tu), false));
                    if let Some(&bi) = tr_indices.get(&tu.id) {
                        consumed_tr.insert(bi);
                    }
                } else if tu.name == "wait_agents" {
                    block_lines.extend(render_wait_agents(tu, false));
                    if let Some(&bi) = tr_indices.get(&tu.id) {
                        consumed_tr.insert(bi);
                    }
                } else if is_special_tool(tu) {
                    // 特殊交互工具（ask_user/todo_write/view_image）保持详细渲染
                    let tool_pause = state.tool_pause_for_tool_use(&tu.id);
                    let tool_pause_active =
                        tool_pause.map(|pause| state.is_active_tool_pause(pause));
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
                } else {
                    let tool_pause = state.tool_pause_for_tool_use(&tu.id);
                    let tool_pause_active =
                        tool_pause.map(|pause| state.is_active_tool_pause(pause));
                    // 检查是否有对应的 ToolResult
                    let tr = tr_indices.get(&tu.id).and_then(|&bi| {
                        if let ContentBlock::ToolResult(tr) = &pending.content[bi] {
                            consumed_tr.insert(bi);
                            Some(tr.clone())
                        } else {
                            None
                        }
                    });
                    if tr.is_none() && tool_pause.is_some() {
                        // 权限确认等待期间保持带确认预览的等待渲染
                        block_lines.extend(render_tool(
                            tu,
                            None,
                            tool_pause,
                            tool_pause_active,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        ));
                    } else {
                        // 常规工具紧凑形态：主行 + 结果首行。
                        block_lines.extend(render_tool_compact(
                            tu,
                            tr.as_ref(),
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        ));
                    }
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

fn read_task_label<'a>(state: &'a UiState, tool_use: &ToolUseBlock) -> Option<(&'a str, &'a str)> {
    let task_id = tool_use.input.get("task_id")?.as_str()?;
    state
        .subagents
        .values()
        .find(|node| node.task_id == task_id)
        .map(|node| (node.agent_label.as_str(), node.title.as_str()))
}

/// 渲染 assistant 消息的活动聚合摘要块：`⏺ Thought for 12s, ran 1 shell command`
/// 常规工具计数合并在摘要主行中；配对结果不额外显示错误预览。
///
/// thinking 时长取消息内所有已测量 Thinking 块之和（旧记录无数据则不计）；
/// 常规工具按类别统计，其配对的 ToolResult 位置标记进 `consumed` 防止重复渲染。
/// 无思考数据且无常规工具时返回空（如纯文本回复或仅特殊工具）。
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
    use omini_domain::message::ThinkingBlock;
    use omini_domain::message::{ContentBlock, Message, Role};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn thought_and_shell_call(duration_ms: u64, id: &str, output: &str) -> Message {
        let input = HashMap::from([("command".to_string(), serde_json::json!("pwd"))]);
        Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Thinking(ThinkingBlock {
                    thinking: "hidden thought".to_string(),
                    duration_ms: Some(duration_ms),
                }),
                ContentBlock::from_tool_use(id.to_string(), "bash".to_string(), input),
                ContentBlock::from_tool_result(id.to_string(), false, output.to_string()),
            ],
        )
    }

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
            vec!["⏺ 主消息", "  └ ok", "    中文abcdef", "    done"]
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
    fn thinking_duration_renders_in_activity_summary() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.messages.push(UiMessage::Message(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Thinking(ThinkingBlock {
                    thinking: "checking context".to_string(),
                    duration_ms: Some(5300),
                }),
                ContentBlock::from_text("done".to_string()),
            ],
        )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 12)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert!(rendered.contains("Thought for 5s"), "rendered: {rendered}");
        // 思考内容文本不再展示
        assert!(!rendered.contains("checking context"));
        assert!(rendered.contains("done"));
    }

    #[test]
    fn thinking_without_duration_renders_no_thinking_line() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        // 旧持久化记录：无 duration_ms，也没有工具活动 → 不渲染任何思考行
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

        let rendered = state.selectable_message_lines.join("\n");
        assert!(!rendered.contains("Thought for"));
        assert!(!rendered.contains("checking context"));
        assert!(rendered.contains("done"));
    }

    #[test]
    fn activity_summary_aggregates_thinking_and_regular_tools() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        let bash_input = HashMap::from([("command".to_string(), serde_json::json!("git status"))]);
        state.messages.push(UiMessage::Message(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Thinking(ThinkingBlock {
                    thinking: "let me check".to_string(),
                    duration_ms: Some(12_000),
                }),
                ContentBlock::from_tool_use("t1".to_string(), "bash".to_string(), bash_input),
                ContentBlock::from_tool_result(
                    "t1".to_string(),
                    false,
                    "On branch main\n".to_string(),
                ),
                ContentBlock::from_text("fixed".to_string()),
            ],
        )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert!(
            rendered.contains("Thought for 12s, ran 1 shell command"),
            "rendered: {rendered}"
        );
        assert!(!rendered.contains("On branch main"));
        assert!(rendered.contains("fixed"));
        // 常规工具不再展开独立主行
        assert!(!rendered.contains("⏺ Bash"));
    }

    #[test]
    fn adjacent_tool_turns_share_one_thought_activity_summary() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.messages.extend([
            UiMessage::Message(thought_and_shell_call(5_300, "t1", "first output")),
            UiMessage::Message(thought_and_shell_call(6_700, "t2", "second output")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 12s, ran 2 shell commands"),
            "{rendered}"
        );
        assert!(!rendered.contains("first output"), "{rendered}");
        assert!(!rendered.contains("middle output"), "{rendered}");
        assert!(!rendered.contains("second output"), "{rendered}");
        assert!(!rendered.contains("hidden thought"));
    }

    #[test]
    fn incrementally_appended_activity_stays_one_uncached_group() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state
            .messages
            .push(UiMessage::Message(thought_and_shell_call(
                5_000,
                "t1",
                "first output",
            )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        assert_eq!(state.render_cache.completed_message_count, 0);

        state.messages.push(UiMessage::Message(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::from_tool_use("t2".into(), "bash".into(), HashMap::new()),
                ContentBlock::from_tool_result("t2".into(), false, "middle output".into()),
            ],
        )));
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        assert_eq!(state.render_cache.completed_message_count, 0);

        state
            .messages
            .push(UiMessage::Message(thought_and_shell_call(
                7_000,
                "t3",
                "second output",
            )));
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 12s, ran 3 shell commands"),
            "{rendered}"
        );
        assert!(!rendered.contains("first output"), "{rendered}");
        assert!(!rendered.contains("second output"), "{rendered}");
        assert_eq!(state.render_cache.completed_message_count, 0);
    }

    #[test]
    fn restored_thread_snapshot_collapses_consecutive_thought_and_tool_messages() {
        let backend = TestBackend::new(100, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        let read_turn = |id: &str, duration_ms: Option<u64>, narration: Option<&str>| {
            let mut content = Vec::new();
            if let Some(duration_ms) = duration_ms {
                content.push(ContentBlock::Thinking(ThinkingBlock {
                    thinking: "hidden thought".into(),
                    duration_ms: Some(duration_ms),
                }));
            }
            if let Some(narration) = narration {
                content.push(ContentBlock::from_text(narration.into()));
            }
            content.push(ContentBlock::from_tool_use(
                id.into(),
                "read".into(),
                HashMap::new(),
            ));
            Message::new(Role::Assistant, content)
        };
        let tool_result = |id: &str, content: &str| {
            Message::new(
                Role::User,
                vec![ContentBlock::from_tool_result(
                    id.into(),
                    false,
                    content.into(),
                )],
            )
        };
        state.apply_thread_snapshot(
            Some("restored-thread".into()),
            vec![
                omini_domain::display::HistoryItem::Message(read_turn("r1", Some(5_000), None)),
                omini_domain::display::HistoryItem::Message(tool_result("r1", "101: text,")),
                omini_domain::display::HistoryItem::Message(read_turn("r2", None, None)),
                omini_domain::display::HistoryItem::Message(tool_result("r2", "880: text,")),
                omini_domain::display::HistoryItem::Message(read_turn(
                    "r3",
                    Some(7_000),
                    Some("检查剩余的状态逻辑。"),
                )),
                omini_domain::display::HistoryItem::Message(tool_result(
                    "r3",
                    "225: if hours > 0 {",
                )),
            ],
            Vec::new(),
            crate::types::events::ThreadUsageSnapshot::default(),
        );

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 100, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 12s, read 3 files"),
            "{rendered}"
        );
        assert!(!rendered.contains("text,"), "{rendered}");
        assert!(!rendered.contains("if hours > 0"), "{rendered}");
        assert!(rendered.contains("检查剩余的状态逻辑。"), "{rendered}");
    }

    #[test]
    fn thought_before_ask_user_joins_prior_activity_but_keeps_prompt_visible() {
        let backend = TestBackend::new(100, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        let shell_input = HashMap::from([("command".into(), serde_json::json!("ls"))]);
        let ask_input = HashMap::new();
        state.apply_thread_snapshot(
            Some("thread-with-ask-user".into()),
            vec![
                omini_domain::display::HistoryItem::Message(Message::new(
                    Role::Assistant,
                    vec![
                        ContentBlock::Thinking(ThinkingBlock {
                            thinking: "checking the project".into(),
                            duration_ms: Some(1_040),
                        }),
                        ContentBlock::from_tool_use("shell-1".into(), "bash".into(), shell_input),
                    ],
                )),
                omini_domain::display::HistoryItem::Message(Message::new(
                    Role::User,
                    vec![ContentBlock::from_tool_result(
                        "shell-1".into(),
                        false,
                        "Cargo.toml".into(),
                    )],
                )),
                omini_domain::display::HistoryItem::Message(Message::new(
                    Role::Assistant,
                    vec![
                        ContentBlock::Thinking(ThinkingBlock {
                            thinking: "deciding what to clarify".into(),
                            duration_ms: Some(161),
                        }),
                        ContentBlock::from_text("请确认 review 范围。".into()),
                        ContentBlock::from_tool_use("ask-1".into(), "ask_user".into(), ask_input),
                    ],
                )),
            ],
            Vec::new(),
            crate::types::events::ThreadUsageSnapshot::default(),
        );

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 100, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 1s, ran 1 shell command"),
            "{rendered}"
        );
        assert!(rendered.contains("请确认 review 范围。"), "{rendered}");
    }

    #[test]
    fn adjacent_thoughts_merge_across_tool_only_turns_without_output_previews() {
        let backend = TestBackend::new(100, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        let blank_message =
            Message::new(Role::Assistant, vec![ContentBlock::from_text(" \n".into())]);
        let search_call = |id: &str, output: &str| {
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::Thinking(ThinkingBlock {
                        thinking: "hidden thought".into(),
                        duration_ms: Some(1_000),
                    }),
                    ContentBlock::from_tool_use(id.into(), "search".into(), HashMap::new()),
                    ContentBlock::from_tool_result(id.into(), false, output.into()),
                ],
            )
        };
        let search_only = |id: &str, output: &str| {
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_tool_use(id.into(), "search".into(), HashMap::new()),
                    ContentBlock::from_tool_result(id.into(), false, output.into()),
                ],
            )
        };
        state.messages.extend([
            UiMessage::Message(search_call("s1", "Found 4 matches")),
            UiMessage::Message(blank_message),
            UiMessage::Message(search_only("s2", "Found 9 matches")),
            UiMessage::Message(search_call("s3", "Found 3 matches")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 100, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 2s, ran 3 searches"),
            "{rendered}"
        );
        assert!(!rendered.contains("Found "), "{rendered}");
        assert_eq!(rendered.matches("└ ").count(), 0, "{rendered}");
    }

    #[test]
    fn visible_assistant_text_closes_thought_activity_group() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.messages.extend([
            UiMessage::Message(thought_and_shell_call(5_000, "t1", "first output")),
            UiMessage::Message(Message::new(
                Role::Assistant,
                vec![ContentBlock::from_text("intermediate answer".to_string())],
            )),
            UiMessage::Message(thought_and_shell_call(7_000, "t2", "second output")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 2, "{rendered}");
        assert!(rendered.contains("intermediate answer"));
        assert!(rendered.contains("Thought for 5s"));
        assert!(rendered.contains("Thought for 7s"));
    }

    #[test]
    fn user_message_closes_thought_activity_group() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.messages.extend([
            UiMessage::Message(thought_and_shell_call(5_000, "t1", "first output")),
            UiMessage::Message(Message::new(
                Role::User,
                vec![ContentBlock::from_text("follow-up".to_string())],
            )),
            UiMessage::Message(thought_and_shell_call(7_000, "t2", "second output")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 2, "{rendered}");
        assert!(rendered.contains("follow-up"));
    }

    #[test]
    fn errored_tool_result_is_omitted_from_merged_activity_preview() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        let input = HashMap::from([("command".to_string(), serde_json::json!("false"))]);
        state.messages.push(UiMessage::Message(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Thinking(ThinkingBlock {
                    thinking: "hidden".to_string(),
                    duration_ms: Some(2_000),
                }),
                ContentBlock::from_tool_use("t1".to_string(), "bash".to_string(), input),
                ContentBlock::from_tool_result(
                    "t1".to_string(),
                    true,
                    "command failed".to_string(),
                ),
            ],
        )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 12)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert!(
            rendered.contains("Thought for 2s, ran 1 shell command"),
            "{rendered}"
        );
        assert!(!rendered.contains("└ error:"), "{rendered}");
        assert!(!rendered.contains("command failed"), "{rendered}");
        assert!(!rendered.contains("hidden"));
    }

    #[test]
    fn active_thought_merges_with_recent_tool_activity_and_keeps_live_time() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.main_query_active = true;
        state
            .messages
            .push(UiMessage::Message(thought_and_shell_call(
                5_000,
                "t1",
                "first output",
            )));
        state.pending_assistant = Some(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::from_thinking("still hidden".to_string()),
                ContentBlock::from_tool_use(
                    "t2".to_string(),
                    "bash".to_string(),
                    HashMap::from([("command".to_string(), serde_json::json!("pwd"))]),
                ),
            ],
        ));
        state.thinking_started_at = Some(std::time::Instant::now() - Duration::from_secs(2));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 7s, ran 2 shell commands"),
            "{rendered}"
        );
        assert!(!rendered.contains("first output"), "{rendered}");
        assert!(!rendered.contains("still hidden"));
    }

    #[test]
    fn active_thought_merges_before_the_visible_answer_closes_the_group() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.main_query_active = true;
        state
            .messages
            .push(UiMessage::Message(thought_and_shell_call(
                5_000,
                "t1",
                "first output",
            )));
        state.pending_assistant = Some(Message::new(
            Role::Assistant,
            vec![
                ContentBlock::from_thinking("hidden continuation".to_string()),
                ContentBlock::from_text("final answer".to_string()),
            ],
        ));
        state.thinking_started_at = Some(std::time::Instant::now() - Duration::from_secs(2));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.selectable_message_lines.join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 7s, ran 1 shell command"),
            "{rendered}"
        );
        assert!(rendered.contains("final answer"));
        assert!(!rendered.contains("hidden continuation"));
    }
}
