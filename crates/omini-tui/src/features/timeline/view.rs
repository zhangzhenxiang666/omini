use crate::app::event::{Notification, NotificationKind};
use crate::app::state::{UiMessage, UiSystemEvent, format_run_duration};
use crate::features::timeline::model::UserDraft;
use crate::features::tools::{
    build_bordered_lines, format_thinking_duration, render_tool, tool_error_display_text,
    truncate_display_width,
};
use crate::ui::context::ViewContext;
use crate::ui::prelude::activity::ActivityGroup;
use crate::ui::prelude::{
    build_assistant_text_lines, build_llm_summary_lines, build_proposed_plan_lines,
    line_to_plain_text, line_width, styled_wrapped_draft, truncate_str,
};
use crate::ui::theme::USER_MESSAGE_BG;
use omini_domain::task::TaskStatus;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

fn build_user_draft_lines(draft: &UserDraft, content_width: usize) -> Vec<Line<'static>> {
    let user_bg = USER_MESSAGE_BG;
    let bg_style = Style::default().bg(user_bg);
    let mut lines = Vec::new();

    let wrapped = styled_wrapped_draft(draft, content_width.saturating_sub(2), bg_style);
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
        NotificationKind::Info => crate::ui::theme::RUNNING,
        NotificationKind::Warn => crate::ui::theme::ACCENT,
        NotificationKind::Error => crate::ui::theme::ERROR,
    };
    let style = Style::default().fg(color);
    let detail_style = Style::default().fg(crate::ui::theme::MUTED);
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
        let wrapped = crate::features::tools::word_wrap(&notification.message, wrap_width);
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
    let style = Style::default().fg(crate::ui::theme::BORDER);
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

pub fn render_messages(state: &mut ViewContext<'_>, frame: &mut ratatui::Frame, area: Rect) {
    if state.session.messages.is_empty()
        && state.session.pending_assistant.is_none()
        && state.session.pending_proposed_plan.is_none()
        && state.session.pending_compact_summary.is_none()
    {
        state.viewport.selectable_message_lines.clear();
        state.viewport.message_scroll_y = 0;
        return;
    }

    let content_width = area.width as usize;
    let visible_height = area.height as usize;

    // 历史与流式尾部每帧经同一投影器渲染，避免不同缓存路径改变聚合边界。
    let timeline_lines = render_message_range(state, content_width);

    let plan_lines = state
        .session
        .pending_proposed_plan
        .as_ref()
        .filter(|plan| !plan.trim().is_empty())
        .map(|plan| render_pending_plan_lines(plan, content_width))
        .unwrap_or_default();

    let compact_lines = state
        .session
        .pending_compact_summary
        .as_ref()
        .map(|text| render_pending_compact_lines(text, content_width))
        .unwrap_or_default();

    let n_timeline = timeline_lines.0.len();
    let n_plan = plan_lines.0.len();
    let n_compact = compact_lines.0.len();
    let has_timeline_plan_separator = n_timeline > 0 && n_plan > 0;
    let plan_offset = n_timeline + has_timeline_plan_separator as usize;
    let has_content_before_compact = n_timeline > 0 || n_plan > 0;
    let has_compact_separator = n_compact > 0 && has_content_before_compact;
    let compact_offset = plan_offset + n_plan + has_compact_separator as usize;
    let total_lines = compact_offset + n_compact;

    let prev_total_lines = state.viewport.total_lines;
    state.viewport.total_lines = total_lines;
    if total_lines == 0 {
        return;
    }

    if !state.viewport.auto_scroll {
        let delta = total_lines.saturating_sub(prev_total_lines);
        state.viewport.scroll_offset = state.viewport.scroll_offset.saturating_add(delta);
    }

    let max_scroll = total_lines.saturating_sub(visible_height);
    let capped_offset = state.viewport.scroll_offset.min(max_scroll);
    state.viewport.scroll_offset = capped_offset;
    let scroll_y = max_scroll.saturating_sub(capped_offset);
    state.viewport.message_scroll_y = scroll_y;

    state.viewport.selectable_message_lines.clear();
    state
        .viewport
        .selectable_message_lines
        .extend_from_slice(&timeline_lines.1);
    if has_timeline_plan_separator {
        state.viewport.selectable_message_lines.push(String::new());
    }
    state
        .viewport
        .selectable_message_lines
        .extend_from_slice(&plan_lines.1);
    if has_compact_separator {
        state.viewport.selectable_message_lines.push(String::new());
    }
    state
        .viewport
        .selectable_message_lines
        .extend_from_slice(&compact_lines.1);

    let visible_selectable_lines = state
        .viewport
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

    let render_ctx = SectionRenderContext {
        scroll_y,
        visible_height,
        area,
        user_bg: USER_MESSAGE_BG,
        user_line_bg: Style::default()
            .fg(crate::ui::theme::TEXT)
            .bg(USER_MESSAGE_BG),
    };

    let buf = frame.buffer_mut();

    render_line_section(&timeline_lines.0, 0, &render_ctx, buf);

    if has_timeline_plan_separator {
        render_blank_separator(n_timeline, scroll_y, visible_height, area, buf);
    }

    render_line_section(&plan_lines.0, plan_offset, &render_ctx, buf);

    if has_compact_separator {
        render_blank_separator(plan_offset + n_plan, scroll_y, visible_height, area, buf);
    }

    render_line_section(&compact_lines.0, compact_offset, &render_ctx, buf);
}

/// 渲染时间线段所需的上下文参数。
struct SectionRenderContext {
    scroll_y: usize,
    visible_height: usize,
    area: Rect,
    user_bg: Color,
    user_line_bg: Style,
}

/// 将一段消息直接渲染到 buffer，不拷贝 `Line`。
fn render_line_section(
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

/// 按同一条有序时间线扫描已提交历史与流式尾部，并在明确分界处结算活动摘要。
fn render_message_range(
    state: &ViewContext<'_>,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let mut all_lines: Vec<Line> = Vec::new();
    let mut selectable_lines: Vec<String> = Vec::new();
    let projection = crate::features::timeline::projection::TimelineProjection::new(state.session);
    let mut activity = ActivityGroup::default();
    for entry in &projection.entries {
        match entry {
            crate::features::timeline::projection::TimelineEntry::Blocks(blocks, pending) => {
                let active_thinking = blocks.iter().rposition(|block| {
                    matches!(
                        block,
                        crate::features::timeline::projection::BlockView::Thinking(None)
                    )
                });
                for (index, block) in blocks.iter().enumerate() {
                    render_block(
                        state,
                        block,
                        *pending && Some(index) == active_thinking,
                        &projection,
                        &mut activity,
                        content_width,
                        &mut all_lines,
                        &mut selectable_lines,
                    );
                }
            }
            crate::features::timeline::projection::TimelineEntry::Boundary(message) => {
                flush_activity_group(
                    &mut activity,
                    content_width,
                    &mut all_lines,
                    &mut selectable_lines,
                );
                if let UiMessage::UserInput(input) = message {
                    render_user_message(
                        &crate::features::timeline::model::user_input_draft(input),
                        content_width,
                        &mut all_lines,
                        &mut selectable_lines,
                    );
                } else {
                    let (lines, selectable) = render_ui_boundary(message, state, content_width);
                    append_message_lines(&mut all_lines, &mut selectable_lines, lines, selectable);
                }
            }
        }
    }

    flush_activity_group(
        &mut activity,
        content_width,
        &mut all_lines,
        &mut selectable_lines,
    );
    (all_lines, selectable_lines)
}

fn render_user_message(
    draft: &UserDraft,
    content_width: usize,
    all_lines: &mut Vec<Line<'static>>,
    selectable_lines: &mut Vec<String>,
) {
    append_rendered_lines(
        all_lines,
        selectable_lines,
        build_user_draft_lines(draft, content_width),
    );
}

fn render_ui_boundary(
    ui_message: &UiMessage,
    state: &ViewContext<'_>,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let lines = match ui_message {
        UiMessage::SystemEvent(UiSystemEvent::RunDivider { elapsed }) => {
            build_run_divider_line(*elapsed, content_width)
        }
        UiMessage::SystemEvent(UiSystemEvent::Plan { text }) => {
            build_proposed_plan_lines(text, content_width)
        }
        UiMessage::SystemEvent(UiSystemEvent::Summary { text }) => {
            build_llm_summary_lines(text, content_width)
        }
        UiMessage::SystemEvent(UiSystemEvent::Notification(notification)) => {
            build_notification_lines(notification, content_width)
        }
        UiMessage::SystemEvent(UiSystemEvent::UserInputEcho(draft)) => {
            build_user_draft_lines(draft, content_width)
        }
        UiMessage::SystemEvent(UiSystemEvent::TaskNotification(notification)) => notification
            .tasks
            .iter()
            .flat_map(|task| {
                let is_completed_agent = task.kind == omini_domain::task::TaskKind::SubAgent
                    && task.status == TaskStatus::Completed;
                let node = (task.kind == omini_domain::task::TaskKind::SubAgent)
                    .then(|| {
                        state
                            .sessions
                            .subagents
                            .values()
                            .find(|node| node.task_id == task.task_id)
                    })
                    .flatten();
                let line = if task.kind == omini_domain::task::TaskKind::SubAgent {
                    let title = node.map_or(task.title.as_str(), |node| node.title.as_str());
                    let status = match task.status {
                        TaskStatus::Completed => "finished",
                        TaskStatus::Failed => "failed",
                        TaskStatus::Cancelled => "cancelled",
                        TaskStatus::Interrupted => "interrupted",
                        TaskStatus::Running => "running",
                        TaskStatus::Cancelling => "cancelling",
                    };
                    let duration = node
                        .and_then(|node| node.duration)
                        .or_else(|| {
                            state
                                .sessions
                                .subagent_completion_durations
                                .get(&task.task_id)
                                .copied()
                        })
                        .map(|duration| {
                            let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                            format!(" · {}", format_thinking_duration(millis))
                        })
                        .unwrap_or_default();
                    let text = truncate_display_width(
                        &format!("● Agent \"{title}\" {status}{duration}"),
                        content_width,
                    );
                    let (indicator, body) = if let Some(body) = text.strip_prefix('●') {
                        ("●", body)
                    } else {
                        ("", text.as_str())
                    };
                    Line::from(vec![
                        Span::styled(
                            indicator.to_string(),
                            Style::default()
                                .fg(crate::ui::theme::SUCCESS)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            body.to_string(),
                            Style::default()
                                .fg(crate::ui::theme::MUTED)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ])
                } else {
                    let text = format!(
                        "background task · {} · {} · {} · {}",
                        task.kind.as_str(),
                        task.label,
                        task.title,
                        task.status.as_str()
                    );
                    Line::from(Span::styled(
                        truncate_str(&text, content_width),
                        Style::default().fg(crate::ui::theme::MUTED),
                    ))
                };
                if is_completed_agent {
                    vec![line, Line::from("")]
                } else {
                    vec![line]
                }
            })
            .collect(),
        UiMessage::SystemEvent(UiSystemEvent::ToolResults { .. })
        | UiMessage::UserInput(_)
        | UiMessage::AssistantMessage(_) => Vec::new(),
    };
    let selectable = lines.iter().map(line_to_plain_text).collect();
    (lines, selectable)
}

#[allow(clippy::too_many_arguments)]
fn render_block(
    state: &ViewContext<'_>,
    block: &crate::features::timeline::projection::BlockView<'_>,
    active_tail: bool,
    projection: &crate::features::timeline::projection::TimelineProjection<'_>,
    activity: &mut ActivityGroup,
    content_width: usize,
    all_lines: &mut Vec<Line<'static>>,
    selectable_lines: &mut Vec<String>,
) {
    use crate::features::timeline::projection::BlockView;
    match block {
        BlockView::Thinking(duration) => {
            activity.add_thinking(*duration);
            if active_tail
                && duration.is_none()
                && let Some(started) = state.session.thinking_started_at
            {
                activity.set_active_thinking(
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
            }
        }
        BlockView::ToolUse(tool) => {
            if crate::features::tools::is_hidden_tool(&tool.name) {
                return;
            }
            if !crate::features::timeline::activity::is_activity_boundary_tool(tool) {
                activity.add_tool(tool);
                return;
            }
            flush_activity_group(activity, content_width, all_lines, selectable_lines);
            let result = projection
                .results
                .get(&tool.id)
                .map(|result| result.as_ref());
            let lines = render_tool(
                tool,
                result,
                None,
                None,
                content_width,
                Some(state.project.status_bar.cwd.as_path()),
            );
            append_rendered_lines(all_lines, selectable_lines, lines);
        }
        BlockView::Text(text) if !text.trim().is_empty() => {
            flush_activity_group(activity, content_width, all_lines, selectable_lines);
            append_rendered_lines(
                all_lines,
                selectable_lines,
                build_assistant_text_lines(text, content_width),
            );
        }
        BlockView::Result(result) if !projection.tool_ids.contains(&result.tool_use_id) => {
            activity.orphan_results.push(result.as_ref().clone());
        }
        _ => {}
    }
}

fn flush_activity_group(
    activity: &mut ActivityGroup,
    content_width: usize,
    all_lines: &mut Vec<Line<'static>>,
    selectable_lines: &mut Vec<String>,
) {
    if activity.is_empty() {
        return;
    }
    let group = std::mem::take(activity);
    append_rendered_lines(all_lines, selectable_lines, group.summary(content_width));
    for result in group.orphan_results {
        let color = if result.is_error {
            crate::ui::theme::ERROR
        } else {
            crate::ui::theme::SUCCESS
        };
        let content = if result.is_error {
            tool_error_display_text(&result.content)
        } else {
            result.content
        };
        append_rendered_lines(
            all_lines,
            selectable_lines,
            build_bordered_lines(&content, content_width, color, false, None),
        );
    }
}

fn append_rendered_lines(
    all_lines: &mut Vec<Line<'static>>,
    selectable_lines: &mut Vec<String>,
    lines: Vec<Line<'static>>,
) {
    let selectable = lines.iter().map(line_to_plain_text).collect();
    append_message_lines(all_lines, selectable_lines, lines, selectable);
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
    if !all_lines.is_empty()
        && !all_lines
            .last()
            .is_some_and(|line| line_to_plain_text(line).is_empty())
    {
        all_lines.push(Line::from(""));
        selectable_lines.push(String::new());
    }
    all_lines.append(&mut lines);
    selectable_lines.append(&mut selectable);
}

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
    use crate::app::event::*;
    use crate::app::state::AppState;
    use std::collections::HashMap;
    fn render_message_range(state: &AppState, width: usize) -> (Vec<Line<'static>>, Vec<String>) {
        super::render_message_range(&ViewContext::new(state), width)
    }
    fn render_messages(state: &mut AppState, frame: &mut ratatui::Frame, area: Rect) {
        let mut context = ViewContext::new(state);
        super::render_messages(&mut context, frame, area);
        state.apply_frame(context.finish());
    }

    use omini_domain::conversation::{
        AssistantMessage, AssistantMessageBlock, SystemEvent, ToolResultRecord, UserInput,
    };
    use omini_domain::input::{InputPart, UserInputIntent};
    use omini_model::message::ThinkingBlock;
    use omini_model::message::{ContentBlock, Message, Role};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn history_item_from_model_message(message: Message) -> omini_protocol::HistoryItem {
        match message.role {
            Role::Assistant => {
                let blocks = message
                    .content
                    .into_iter()
                    .filter_map(|block| match block {
                        ContentBlock::Thinking(block) => Some(AssistantMessageBlock::Thinking {
                            thinking: block.thinking,
                            duration_ms: block.duration_ms,
                        }),
                        ContentBlock::Text(block) => {
                            Some(AssistantMessageBlock::Text { text: block.text })
                        }
                        ContentBlock::ToolUse(block) => Some(AssistantMessageBlock::ToolUse {
                            id: block.id,
                            name: block.name,
                            input: block.input,
                        }),
                        ContentBlock::Image(_) | ContentBlock::ToolResult(_) => None,
                    })
                    .collect();
                omini_protocol::HistoryItem::AssistantMessage(AssistantMessage { blocks })
            }
            Role::User => {
                let results = message
                    .content
                    .into_iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult(result) => Some(ToolResultRecord {
                            tool_use_id: result.tool_use_id,
                            is_error: result.is_error,
                            content: result.content,
                            metadata: result.metadata,
                        }),
                        _ => None,
                    })
                    .collect();
                omini_protocol::HistoryItem::SystemEvent(SystemEvent::ToolResults { results })
            }
        }
    }

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

    fn rendered_timeline(state: &AppState) -> String {
        render_message_range(state, 100)
            .0
            .iter()
            .map(line_to_plain_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assistant_item(message: Message) -> UiMessage {
        UiMessage::from_model_message(message)
            .into_iter()
            .find(|item| matches!(item, UiMessage::AssistantMessage(_)))
            .expect("assistant model message should produce a history item")
    }

    fn user_input_item(text: &str) -> UiMessage {
        UiMessage::UserInput(UserInput {
            intent: UserInputIntent::Message,
            parts: vec![InputPart::Text {
                text: text.to_string(),
            }],
            attachments: Vec::new(),
        })
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

        assert_eq!(lines[0].spans[0].style.fg, Some(crate::ui::theme::ERROR));
    }

    #[test]
    fn thinking_duration_renders_in_activity_summary() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
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

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(rendered.contains("Thought for 5s"), "rendered: {rendered}");
        // 思考内容文本不再展示
        assert!(!rendered.contains("checking context"));
        assert!(rendered.contains("done"));
    }

    #[test]
    fn thinking_without_duration_renders_no_thinking_line() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        // 旧持久化记录：无 duration_ms，也没有工具活动 → 不渲染任何思考行
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_thinking("checking context".to_string()),
                    ContentBlock::from_text("done".to_string()),
                ],
            )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 12)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(!rendered.contains("Thought for"));
        assert!(!rendered.contains("checking context"));
        assert!(rendered.contains("done"));
    }

    #[test]
    fn activity_summary_aggregates_thinking_and_regular_tools() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        let bash_input = HashMap::from([("command".to_string(), serde_json::json!("git status"))]);
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
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

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
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
    fn orchestration_visibility() {
        let mut state = AppState::new();
        let blocks = vec![
            ContentBlock::Thinking(ThinkingBlock {
                thinking: "hidden reasoning".to_string(),
                duration_ms: Some(2_000),
            }),
            ContentBlock::from_tool_use("run-1".into(), "run_agent".into(), HashMap::new()),
            ContentBlock::from_tool_use("read-1".into(), "read_task".into(), HashMap::new()),
            ContentBlock::from_tool_use("cancel-1".into(), "cancel_task".into(), HashMap::new()),
            ContentBlock::from_tool_use(
                "spawn-1".into(),
                "spawn_agent".into(),
                HashMap::from([
                    ("name".into(), serde_json::json!("Explore")),
                    ("title".into(), serde_json::json!("Search architecture")),
                ]),
            ),
            ContentBlock::from_tool_use("run-2".into(), "run_agent".into(), HashMap::new()),
        ];
        let results = ["run-1", "read-1", "cancel-1", "spawn-1", "run-2"]
            .into_iter()
            .map(|id| ContentBlock::from_tool_result(id.into(), false, "hidden result".into()))
            .collect::<Vec<_>>();
        state.sessions.views["main"]
            .messages
            .extend(UiMessage::from_model_message(Message::new(
                Role::Assistant,
                blocks,
            )));
        state.sessions.views["main"]
            .messages
            .extend(UiMessage::from_model_message(Message::new(
                Role::User,
                results,
            )));
        state
            .sessions
            .subagents_by_tool_use
            .insert("spawn-1".into(), "thread-1".into());
        state.sessions.subagents.insert(
            "thread-1".into(),
            crate::features::sessions::model::SubagentNode {
                task_id: "task-1".into(),
                thread_id: "thread-1".into(),
                parent_thread_id: "main-thread".into(),
                spawn_tool_use_id: "spawn-1".into(),
                agent_label: "Explore".into(),
                title: "Search architecture".into(),
                execution_mode: crate::app::event::AgentTaskExecutionMode::Background,
                status: omini_domain::task::TaskStatus::Running,
                duration: None,
                started_at: chrono::Utc::now(),
                messages: Vec::new(),
            },
        );
        state.sessions.subagent_order.push("task-1".into());
        let launch_before_completion = rendered_timeline(&state);
        let child = state.sessions.subagents.get_mut("thread-1").unwrap();
        child.status = omini_domain::task::TaskStatus::Completed;
        child.duration = Some(Duration::from_secs(591));
        state.prune_terminal_tasks();
        // 子任务的运行状态变化只影响独立通知，不改写已经显示的调用条目。
        assert_eq!(rendered_timeline(&state), launch_before_completion);
        state.sessions.views["main"]
            .messages
            .push(UiMessage::SystemEvent(UiSystemEvent::TaskNotification(
                omini_domain::conversation::TaskNotification {
                    tasks: vec![omini_domain::task::TaskCompletion {
                        task_id: "task-1".into(),
                        kind: omini_domain::task::TaskKind::SubAgent,
                        label: "Explore".into(),
                        title: "Search architecture".into(),
                        status: omini_domain::task::TaskStatus::Completed,
                        summary: None,
                    }],
                    created_at: chrono::Utc::now(),
                },
            )));
        let rendered = rendered_timeline(&state);
        assert!(rendered.contains("Thought for 2s"), "{rendered}");
        assert!(!rendered.contains("read_task"), "{rendered}");
        assert!(!rendered.contains("cancel_task"), "{rendered}");
        assert!(!rendered.contains("used run_agent"), "{rendered}");
        assert!(!rendered.contains("hidden result"));
        assert!(rendered.contains("● Agent \"Search architecture\" finished · 9m 51s"));
        assert!(rendered.contains("◆ Explore · Search architecture"));
        assert!(!rendered.contains("后台 ·"));
        assert!(!rendered.contains("同步 ·"));
        assert!(!rendered.to_lowercase().contains("ctrl+"));
        assert!(!rendered.contains("to expand"));
        assert!(rendered.contains("Search architecture"));
    }

    #[test]
    fn every_explicit_boundary_splits_groups_inside_one_assistant_message() {
        let mut state = AppState::new();
        let mut blocks = vec![
            ContentBlock::Thinking(ThinkingBlock {
                thinking: "hidden".into(),
                duration_ms: Some(5_000),
            }),
            ContentBlock::from_tool_use("bash-1".into(), "bash".into(), HashMap::new()),
            ContentBlock::from_text("visible narration".into()),
            ContentBlock::from_tool_use("read-1".into(), "read".into(), HashMap::new()),
            ContentBlock::from_tool_use(
                "edit-1".into(),
                "edit".into(),
                HashMap::from([("file_path".into(), serde_json::json!("src/lib.rs"))]),
            ),
            ContentBlock::from_tool_use("bash-2".into(), "bash".into(), HashMap::new()),
            ContentBlock::from_tool_use(
                "write-1".into(),
                "write".into(),
                HashMap::from([("file_path".into(), serde_json::json!("out.txt"))]),
            ),
            ContentBlock::from_tool_use("search-1".into(), "search".into(), HashMap::new()),
            ContentBlock::from_tool_use("ask-1".into(), "ask_user".into(), HashMap::new()),
            ContentBlock::from_tool_use("read-2".into(), "read".into(), HashMap::new()),
            ContentBlock::from_tool_use("todo-1".into(), "todo_write".into(), HashMap::new()),
            ContentBlock::from_tool_use("bash-3".into(), "bash".into(), HashMap::new()),
            ContentBlock::from_tool_use(
                "spawn-1".into(),
                "spawn_agent".into(),
                HashMap::from([
                    ("name".into(), serde_json::json!("Explore")),
                    ("title".into(), serde_json::json!("Find entrypoints")),
                ]),
            ),
            ContentBlock::from_tool_use("search-2".into(), "search".into(), HashMap::new()),
        ];
        blocks.extend(
            [
                "bash-1", "read-1", "edit-1", "bash-2", "write-1", "search-1", "ask-1", "read-2",
                "todo-1", "bash-3", "spawn-1", "search-2",
            ]
            .into_iter()
            .map(|id| ContentBlock::from_tool_result(id.into(), false, "hidden result".into())),
        );
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(Role::Assistant, blocks)));

        let rendered = rendered_timeline(&state);
        let ordered_markers = [
            "Thought for 5s, ran 1 shell command",
            "visible narration",
            "Read 1 file",
            "⏺ Patch",
            "Ran 1 shell command",
            "⏺ Write",
            "Ran 1 search",
            "⏺ Questions",
            "Read 1 file",
            "⏺ Checklist",
            "Ran 1 shell command",
            "◆ Explore · Find entrypoints",
            "Ran 1 search",
        ];
        let mut remaining = rendered.as_str();
        for marker in ordered_markers {
            let Some(position) = remaining.find(marker) else {
                panic!("missing or out-of-order marker {marker:?} in {rendered}");
            };
            remaining = &remaining[position + marker.len()..];
        }
        assert_eq!(
            rendered.matches("Ran 1 shell command").count(),
            2,
            "{rendered}"
        );
        assert_eq!(rendered.matches("Ran 1 search").count(), 2, "{rendered}");
        assert!(!rendered.contains("hidden result"));
    }

    #[test]
    fn all_non_tool_result_ui_events_split_activity_groups() {
        let events = vec![
            UiMessage::SystemEvent(UiSystemEvent::UserInputEcho(
                crate::features::timeline::model::UserDraft::plain("user input".into()),
            )),
            UiMessage::SystemEvent(UiSystemEvent::Plan {
                text: "# Proposed plan".into(),
            }),
            UiMessage::SystemEvent(UiSystemEvent::RunDivider {
                elapsed: Duration::from_secs(1),
            }),
            UiMessage::SystemEvent(UiSystemEvent::Notification(Notification::info(
                "system notice",
            ))),
            UiMessage::SystemEvent(UiSystemEvent::Summary {
                text: "compacted context".into(),
            }),
            UiMessage::SystemEvent(UiSystemEvent::TaskNotification(
                omini_domain::conversation::TaskNotification {
                    tasks: vec![omini_domain::task::TaskCompletion {
                        task_id: "task-1".into(),
                        kind: omini_domain::task::TaskKind::SubAgent,
                        label: "Explore".into(),
                        title: "Search architecture".into(),
                        status: omini_domain::task::TaskStatus::Completed,
                        summary: None,
                    }],
                    created_at: chrono::Utc::now(),
                },
            )),
        ];

        for event in events {
            let is_task_notification = matches!(
                &event,
                UiMessage::SystemEvent(UiSystemEvent::TaskNotification(_))
            );
            let mut state = AppState::new();
            state.sessions.views["main"].messages.extend([
                assistant_item(Message::new(
                    Role::Assistant,
                    vec![ContentBlock::from_tool_use(
                        "read-1".into(),
                        "read".into(),
                        HashMap::new(),
                    )],
                )),
                event,
                assistant_item(Message::new(
                    Role::Assistant,
                    vec![ContentBlock::from_tool_use(
                        "search-1".into(),
                        "search".into(),
                        HashMap::new(),
                    )],
                )),
            ]);

            let rendered = rendered_timeline(&state);
            let read = rendered.find("Read 1 file").expect("read summary");
            let search = rendered.find("Ran 1 search").expect("search summary");
            assert!(read < search, "event failed to split activity: {rendered}");
            assert_eq!(rendered.matches("Read 1 file").count(), 1, "{rendered}");
            assert_eq!(rendered.matches("Ran 1 search").count(), 1, "{rendered}");
            if is_task_notification {
                assert!(
                    rendered.contains("● Agent \"Search architecture\" finished"),
                    "{rendered}"
                );
                assert!(
                    !rendered.contains("background task · sub_agent"),
                    "{rendered}"
                );
            }
        }
    }

    #[test]
    fn adjacent_tool_turns_share_one_thought_activity_summary() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"].messages.extend([
            assistant_item(thought_and_shell_call(5_300, "t1", "first output")),
            assistant_item(thought_and_shell_call(6_700, "t2", "second output")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
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
    fn appended_activity_stays_in_the_same_open_group() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"]
            .messages
            .push(assistant_item(thought_and_shell_call(
                5_000,
                "t1",
                "first output",
            )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_tool_use("t2".into(), "bash".into(), HashMap::new()),
                    ContentBlock::from_tool_result("t2".into(), false, "middle output".into()),
                ],
            )));
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        state.sessions.views["main"]
            .messages
            .push(assistant_item(thought_and_shell_call(
                7_000,
                "t3",
                "second output",
            )));
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 12s, ran 3 shell commands"),
            "{rendered}"
        );
        assert!(!rendered.contains("first output"), "{rendered}");
        assert!(!rendered.contains("second output"), "{rendered}");
    }

    #[test]
    fn restored_thread_snapshot_collapses_consecutive_thought_and_tool_messages() {
        let backend = TestBackend::new(100, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
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
                history_item_from_model_message(read_turn("r1", Some(5_000), None)),
                history_item_from_model_message(tool_result("r1", "101: text,")),
                history_item_from_model_message(read_turn("r2", None, None)),
                history_item_from_model_message(tool_result("r2", "880: text,")),
                history_item_from_model_message(read_turn(
                    "r3",
                    Some(7_000),
                    Some("检查剩余的状态逻辑。"),
                )),
                history_item_from_model_message(tool_result("r3", "225: if hours > 0 {")),
            ],
            Vec::new(),
            crate::app::event::ThreadUsageSnapshot::default(),
        );

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 100, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 12s, read 2 files"),
            "{rendered}"
        );
        assert!(rendered.contains("Read 1 file"), "{rendered}");
        assert!(!rendered.contains("text,"), "{rendered}");
        assert!(!rendered.contains("if hours > 0"), "{rendered}");
        assert!(rendered.contains("检查剩余的状态逻辑。"), "{rendered}");
    }

    #[test]
    fn thought_before_ask_user_joins_prior_activity_but_keeps_prompt_visible() {
        let backend = TestBackend::new(100, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        let shell_input = HashMap::from([("command".into(), serde_json::json!("ls"))]);
        let ask_input = HashMap::new();
        state.apply_thread_snapshot(
            Some("thread-with-ask-user".into()),
            vec![
                history_item_from_model_message(Message::new(
                    Role::Assistant,
                    vec![
                        ContentBlock::Thinking(ThinkingBlock {
                            thinking: "checking the project".into(),
                            duration_ms: Some(1_040),
                        }),
                        ContentBlock::from_tool_use("shell-1".into(), "bash".into(), shell_input),
                    ],
                )),
                history_item_from_model_message(Message::new(
                    Role::User,
                    vec![ContentBlock::from_tool_result(
                        "shell-1".into(),
                        false,
                        "Cargo.toml".into(),
                    )],
                )),
                history_item_from_model_message(Message::new(
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
            crate::app::event::ThreadUsageSnapshot::default(),
        );

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 100, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
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
        let mut state = AppState::new();
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
        state.sessions.views["main"].messages.extend([
            assistant_item(search_call("s1", "Found 4 matches")),
            assistant_item(blank_message),
            assistant_item(search_only("s2", "Found 9 matches")),
            assistant_item(search_call("s3", "Found 3 matches")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 100, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 2s, ran 3 searches"),
            "{rendered}"
        );
        assert!(!rendered.contains("Found "), "{rendered}");
        assert_eq!(rendered.matches("└ ").count(), 0, "{rendered}");
    }

    #[test]
    fn success_and_error_tool_results_do_not_break_or_duplicate_activity() {
        let mut state = AppState::new();
        state.sessions.views["main"]
            .messages
            .extend(UiMessage::from_model_message(Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::Thinking(ThinkingBlock {
                        thinking: "hidden".into(),
                        duration_ms: Some(12_000),
                    }),
                    ContentBlock::from_tool_use("bash-1".into(), "bash".into(), HashMap::new()),
                    ContentBlock::from_tool_use("search-1".into(), "search".into(), HashMap::new()),
                ],
            )));
        state.sessions.views["main"]
            .messages
            .extend(UiMessage::from_model_message(Message::new(
                Role::User,
                vec![
                    ContentBlock::from_tool_result("search-1".into(), true, "search failed".into()),
                    ContentBlock::from_tool_result("bash-1".into(), false, "command output".into()),
                ],
            )));
        state.sessions.views["main"]
            .messages
            .extend(UiMessage::from_model_message(Message::new(
                Role::Assistant,
                vec![ContentBlock::from_tool_use(
                    "read-1".into(),
                    "read".into(),
                    HashMap::new(),
                )],
            )));

        let rendered = rendered_timeline(&state);

        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 12s, ran 1 shell command, ran 1 search, read 1 file"),
            "{rendered}"
        );
        assert!(!rendered.contains("search failed"));
        assert!(!rendered.contains("command output"));
        assert!(!rendered.contains("\n⏺ "));
    }

    #[test]
    fn streaming_tail_and_restored_history_have_identical_groups() {
        let first_turn = Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Thinking(ThinkingBlock {
                    thinking: "hidden".into(),
                    duration_ms: Some(4_000),
                }),
                ContentBlock::from_tool_use("bash-1".into(), "bash".into(), HashMap::new()),
            ],
        );
        let results = Message::new(
            Role::User,
            vec![ContentBlock::from_tool_result(
                "bash-1".into(),
                false,
                "command output".into(),
            )],
        );
        let streaming_tail = Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Thinking(ThinkingBlock {
                    thinking: "still hidden".into(),
                    duration_ms: Some(2_000),
                }),
                ContentBlock::from_tool_use("read-1".into(), "read".into(), HashMap::new()),
                ContentBlock::from_tool_use("search-1".into(), "search".into(), HashMap::new()),
            ],
        );

        let mut live = AppState::new();
        live.sessions.views["main"]
            .messages
            .push(assistant_item(first_turn.clone()));
        live.sessions.views["main"]
            .messages
            .extend(UiMessage::from_model_message(results.clone()));
        live.sessions.views["main"].pending_assistant = Some(streaming_tail.clone().into());

        let mut restored = AppState::new();
        restored.apply_thread_snapshot(
            Some("restored-thread".into()),
            vec![
                history_item_from_model_message(first_turn),
                history_item_from_model_message(results),
                history_item_from_model_message(streaming_tail),
            ],
            Vec::new(),
            crate::app::event::ThreadUsageSnapshot::default(),
        );

        assert_eq!(rendered_timeline(&live), rendered_timeline(&restored));
    }

    #[test]
    fn visible_assistant_text_closes_thought_activity_group() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"].messages.extend([
            assistant_item(thought_and_shell_call(5_000, "t1", "first output")),
            assistant_item(Message::new(
                Role::Assistant,
                vec![ContentBlock::from_text("intermediate answer".to_string())],
            )),
            assistant_item(thought_and_shell_call(7_000, "t2", "second output")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 2, "{rendered}");
        assert!(rendered.contains("intermediate answer"));
        assert!(rendered.contains("Thought for 5s"));
        assert!(rendered.contains("Thought for 7s"));
    }

    #[test]
    fn user_message_closes_thought_activity_group() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"].messages.extend([
            assistant_item(thought_and_shell_call(5_000, "t1", "first output")),
            user_input_item("follow-up"),
            assistant_item(thought_and_shell_call(7_000, "t2", "second output")),
        ]);

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 2, "{rendered}");
        assert!(rendered.contains("follow-up"));
    }

    #[test]
    fn errored_tool_result_is_omitted_from_merged_activity_preview() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        let input = HashMap::from([("command".to_string(), serde_json::json!("false"))]);
        state.sessions.views["main"]
            .messages
            .extend(UiMessage::from_model_message(Message::new(
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

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
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
        let mut state = AppState::new();
        state.sessions.views["main"].main_query_active = true;
        state.sessions.views["main"]
            .messages
            .push(assistant_item(thought_and_shell_call(
                5_000,
                "t1",
                "first output",
            )));
        state.sessions.views["main"].pending_assistant = Some(
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_thinking("still hidden".to_string()),
                    ContentBlock::from_tool_use(
                        "t2".to_string(),
                        "bash".to_string(),
                        HashMap::from([("command".to_string(), serde_json::json!("pwd"))]),
                    ),
                ],
            )
            .into(),
        );
        state.sessions.views["main"].thinking_started_at =
            Some(std::time::Instant::now() - Duration::from_secs(2));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
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
        let mut state = AppState::new();
        state.sessions.views["main"].main_query_active = true;
        state.sessions.views["main"]
            .messages
            .push(assistant_item(thought_and_shell_call(
                5_000,
                "t1",
                "first output",
            )));
        state.sessions.views["main"].pending_assistant = Some(
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_thinking("hidden continuation".to_string()),
                    ContentBlock::from_text("final answer".to_string()),
                ],
            )
            .into(),
        );
        state.sessions.views["main"].thinking_started_at =
            Some(std::time::Instant::now() - Duration::from_secs(2));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();

        let rendered = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert_eq!(rendered.matches("Thought for").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Thought for 7s, ran 1 shell command"),
            "{rendered}"
        );
        assert!(rendered.contains("final answer"));
        assert!(!rendered.contains("hidden continuation"));
    }
}
