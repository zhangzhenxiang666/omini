use crate::app::event::{Notification, NotificationKind};
use crate::app::state::{UiMessage, UiSystemEvent, format_run_duration};
use crate::features::timeline::model::UserDraft;
use crate::features::tools::{
    build_bordered_lines, format_thinking_duration, render_activity_preview, render_tool,
    tool_error_display_text, truncate_display_width,
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
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
struct MessageCheckpoint {
    line_count: usize,
    activity: ActivityGroup,
}

/// 每个会话保留已结算的历史行和未结算的活动组；流式尾部始终从该状态继续。
#[derive(Debug, Default)]
pub struct TimelineRenderCache {
    width: usize,
    project_dir: PathBuf,
    initialized: bool,
    processed_messages: usize,
    lines: Vec<Line<'static>>,
    selectable: Vec<String>,
    activity: ActivityGroup,
    checkpoints: Vec<MessageCheckpoint>,
    results: HashMap<String, omini_model::message::ToolResultBlock>,
    tool_ids: HashSet<String>,
    tool_positions: HashMap<String, usize>,
    result_positions: HashMap<String, usize>,
    pending_result_ids: HashSet<String>,
    pending_tool_ids: HashSet<String>,
    dirty_from: Option<usize>,
    displayed_suffix: Vec<String>,
    #[cfg(test)]
    pub history_passes: usize,
}

impl Clone for TimelineRenderCache {
    fn clone(&self) -> Self {
        // 克隆会话只复制事实状态；派生行由新会话首次绘制时重新建立。
        Self::default()
    }
}

impl TimelineRenderCache {
    /// 非追加改动从受影响消息回退；历史重排和快照替换直接清空索引。
    pub fn mark_dirty(&mut self, index: usize) {
        self.dirty_from = Some(
            self.dirty_from
                .map_or(index, |previous| previous.min(index)),
        );
    }

    pub fn reset(&mut self) {
        let width = self.width;
        let project_dir = self.project_dir.clone();
        #[cfg(test)]
        let history_passes = self.history_passes;
        *self = Self {
            width,
            project_dir,
            #[cfg(test)]
            history_passes,
            ..Self::default()
        };
    }

    /// 先登记新增消息里的工具关联，随后渲染旧调用时即可看到晚到的结果。
    fn index_message(&mut self, message: &UiMessage, index: usize) {
        use crate::features::timeline::projection::{BlockView, TimelineEntry, project_message};
        let TimelineEntry::Blocks(blocks, _) = project_message(message) else {
            return;
        };
        for block in blocks {
            match block {
                BlockView::ToolUse(tool) => {
                    self.tool_ids.insert(tool.id.clone());
                    if let std::collections::hash_map::Entry::Vacant(position) =
                        self.tool_positions.entry(tool.id.clone())
                    {
                        position.insert(index);
                    }
                    if let Some(previous) = self.result_positions.get(&tool.id)
                        && *previous < index
                    {
                        self.mark_dirty(*previous);
                    }
                }
                BlockView::Result(result) => {
                    let id = result.tool_use_id.clone();
                    self.result_positions.entry(id.clone()).or_insert(index);
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        self.results.entry(id.clone())
                    {
                        entry.insert(result.into_owned());
                        if let Some(previous) = self.tool_positions.get(&id) {
                            self.mark_dirty(*previous);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// 只扫描新增消息；旧依赖变化时回到消息检查点，恢复当时的活动组并重放后缀。
    fn prepare(
        &mut self,
        state: &ViewContext<'_>,
        width: usize,
        pending: &PendingLookup<'_>,
    ) -> Option<usize> {
        let project_dir = state.project.status_bar.cwd.as_path();
        if !self.initialized
            || self.width != width
            || self.project_dir != project_dir
            || self.processed_messages > state.session.messages.len()
        {
            self.reset();
            self.width = width;
            self.project_dir = project_dir.to_path_buf();
            self.initialized = true;
            self.mark_dirty(0);
        }

        for index in self.processed_messages..state.session.messages.len() {
            self.index_message(&state.session.messages[index], index);
        }

        // 结果可能在流式尾部先于提交到达，并改变历史里的独立工具行。
        for id in pending.results.keys() {
            if !self.pending_result_ids.contains(*id)
                && !self.results.contains_key(*id)
                && let Some(index) = self.tool_positions.get(*id)
            {
                self.mark_dirty(*index);
            }
        }
        let mut removed_dirty = None;
        for id in &self.pending_result_ids {
            if !pending.results.contains_key(id.as_str())
                && !self.results.contains_key(id)
                && let Some(index) = self.tool_positions.get(id)
            {
                removed_dirty =
                    Some(removed_dirty.map_or(*index, |previous: usize| previous.min(*index)));
            }
        }
        for id in &pending.tool_ids {
            if !self.pending_tool_ids.contains(*id)
                && !self.tool_ids.contains(*id)
                && let Some(index) = self.result_positions.get(*id)
            {
                self.mark_dirty(*index);
            }
        }
        for id in &self.pending_tool_ids {
            if !pending.tool_ids.contains(id.as_str())
                && !self.tool_ids.contains(id)
                && let Some(index) = self.result_positions.get(id)
            {
                removed_dirty = Some(removed_dirty.map_or(*index, |previous| previous.min(*index)));
            }
        }
        if let Some(index) = removed_dirty {
            self.mark_dirty(index);
        }
        self.pending_result_ids = pending.results.keys().map(|id| (*id).to_string()).collect();
        self.pending_tool_ids = pending
            .tool_ids
            .iter()
            .map(|id| (*id).to_string())
            .collect();

        let start = self
            .dirty_from
            .take()
            .unwrap_or(self.processed_messages)
            .min(self.processed_messages);
        if start == state.session.messages.len() && start == self.processed_messages {
            return None;
        }
        let changed_line = if start < self.processed_messages {
            let checkpoint = self.checkpoints[start].clone();
            self.lines.truncate(checkpoint.line_count);
            self.selectable.truncate(checkpoint.line_count);
            self.activity = checkpoint.activity.clone();
            self.checkpoints.truncate(start);
            checkpoint.line_count
        } else {
            self.lines.len()
        };

        let lookup = RenderLookup {
            results: &self.results,
            tool_ids: &self.tool_ids,
            pending,
        };
        for index in start..state.session.messages.len() {
            self.checkpoints.push(MessageCheckpoint {
                line_count: self.lines.len(),
                activity: self.activity.clone(),
            });
            render_entry(
                state,
                &crate::features::timeline::projection::project_message(
                    &state.session.messages[index],
                ),
                &lookup,
                &mut self.activity,
                width,
                &mut self.lines,
                &mut self.selectable,
            );
            #[cfg(test)]
            {
                self.history_passes += 1;
            }
        }
        self.processed_messages = state.session.messages.len();
        Some(changed_line)
    }
}

#[derive(Default)]
struct PendingLookup<'a> {
    results: HashMap<&'a str, &'a omini_model::message::ToolResultBlock>,
    tool_ids: HashSet<&'a str>,
}

impl<'a> PendingLookup<'a> {
    fn new(pending: Option<&'a crate::features::timeline::model::StreamingMessage>) -> Self {
        let mut lookup = Self::default();
        if let Some(pending) = pending {
            for block in &pending.content {
                match block {
                    omini_model::message::ContentBlock::ToolUse(tool) => {
                        lookup.tool_ids.insert(&tool.id);
                    }
                    omini_model::message::ContentBlock::ToolResult(result) => {
                        lookup.results.entry(&result.tool_use_id).or_insert(result);
                    }
                    _ => {}
                }
            }
        }
        lookup
    }
}

struct RenderLookup<'a, 'b> {
    results: &'a HashMap<String, omini_model::message::ToolResultBlock>,
    tool_ids: &'a HashSet<String>,
    pending: &'a PendingLookup<'b>,
}

impl RenderLookup<'_, '_> {
    fn result(&self, id: &str) -> Option<&omini_model::message::ToolResultBlock> {
        self.results
            .get(id)
            .or_else(|| self.pending.results.get(id).copied())
    }

    fn has_tool(&self, id: &str) -> bool {
        self.tool_ids.contains(id) || self.pending.tool_ids.contains(id)
    }
}

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
        let mut cache = state.session.render_cache.borrow_mut();
        if cache.initialized {
            cache.reset();
        }
        if !state.session.selectable_message_lines.is_empty() {
            state.viewport.selectable_patch = Some((0, Vec::new()));
        }
        state.viewport.message_scroll_y = 0;
        state.viewport.total_lines = 0;
        return;
    }

    let content_width = area.width as usize;
    let visible_height = area.height as usize;
    let pending = PendingLookup::new(state.session.pending_assistant.as_ref());
    let mut cache = state.session.render_cache.borrow_mut();
    let changed_line = cache.prepare(state, content_width, &pending);
    let lookup = RenderLookup {
        results: &cache.results,
        tool_ids: &cache.tool_ids,
        pending: &pending,
    };
    let mut tail_lines = Vec::new();
    let mut tail_selectable = Vec::new();
    let mut activity = cache.activity.clone();
    if let Some(pending_message) = state.session.pending_assistant.as_ref() {
        render_entry(
            state,
            &crate::features::timeline::projection::project_pending(pending_message),
            &lookup,
            &mut activity,
            content_width,
            &mut tail_lines,
            &mut tail_selectable,
        );
    }
    let preview = render_active_preview(state, &activity, content_width);
    flush_activity_group(
        &mut activity,
        preview,
        content_width,
        &mut tail_lines,
        &mut tail_selectable,
    );
    if !cache.lines.is_empty()
        && !tail_lines.is_empty()
        && !cache
            .lines
            .last()
            .is_some_and(|line| line_to_plain_text(line).is_empty())
    {
        tail_lines.insert(0, Line::from(""));
        tail_selectable.insert(0, String::new());
    }

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

    let n_stable = cache.lines.len();
    let n_tail = tail_lines.len();
    let n_timeline = n_stable + n_tail;
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

    let mut suffix_selectable = tail_selectable;
    if has_timeline_plan_separator {
        suffix_selectable.push(String::new());
    }
    suffix_selectable.extend_from_slice(&plan_lines.1);
    if has_compact_separator {
        suffix_selectable.push(String::new());
    }
    suffix_selectable.extend_from_slice(&compact_lines.1);
    if changed_line.is_some() || cache.displayed_suffix != suffix_selectable {
        let start = changed_line.unwrap_or(n_stable);
        let mut patch = cache.selectable[start..].to_vec();
        patch.extend_from_slice(&suffix_selectable);
        state.viewport.selectable_patch = Some((start, patch));
        cache.displayed_suffix = suffix_selectable.clone();
    }

    for visible_row in 0..visible_height.min(total_lines.saturating_sub(scroll_y)) {
        let line_index = scroll_y + visible_row;
        let text = if line_index < n_stable {
            &cache.selectable[line_index]
        } else {
            &suffix_selectable[line_index - n_stable]
        };
        state.register_selectable_screen_line(
            area.y + visible_row as u16,
            area.x,
            area.width,
            text.clone(),
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

    render_line_section(&cache.lines, 0, &render_ctx, buf);
    render_line_section(&tail_lines, n_stable, &render_ctx, buf);

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
#[cfg(test)]
fn render_message_range(
    state: &ViewContext<'_>,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let mut all_lines: Vec<Line> = Vec::new();
    let mut selectable_lines: Vec<String> = Vec::new();
    let projection = crate::features::timeline::projection::TimelineProjection::new(state.session);
    let pending = PendingLookup::default();
    let lookup = RenderLookup {
        results: &projection.results,
        tool_ids: &projection.tool_ids,
        pending: &pending,
    };
    let mut activity = ActivityGroup::default();
    for entry in &projection.entries {
        render_entry(
            state,
            entry,
            &lookup,
            &mut activity,
            content_width,
            &mut all_lines,
            &mut selectable_lines,
        );
    }

    let preview = render_active_preview(state, &activity, content_width);
    flush_activity_group(
        &mut activity,
        preview,
        content_width,
        &mut all_lines,
        &mut selectable_lines,
    );
    (all_lines, selectable_lines)
}

/// 全量校验和缓存重放共用此入口，保持消息边界与流式块的分组规则一致。
fn render_entry(
    state: &ViewContext<'_>,
    entry: &crate::features::timeline::projection::TimelineEntry<'_>,
    lookup: &RenderLookup<'_, '_>,
    activity: &mut ActivityGroup,
    content_width: usize,
    all_lines: &mut Vec<Line<'static>>,
    selectable_lines: &mut Vec<String>,
) {
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
                    lookup,
                    activity,
                    content_width,
                    all_lines,
                    selectable_lines,
                );
            }
        }
        crate::features::timeline::projection::TimelineEntry::Boundary(message) => {
            flush_activity_group(activity, None, content_width, all_lines, selectable_lines);
            if let UiMessage::UserInput(input) = message {
                render_user_message(
                    &crate::features::timeline::model::user_input_draft(input),
                    content_width,
                    all_lines,
                    selectable_lines,
                );
            } else {
                let (lines, selectable) = render_ui_boundary(message, state, content_width);
                append_message_lines(all_lines, selectable_lines, lines, selectable);
            }
        }
    }
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

/// 渲染主 Agent 注入子会话的消息：首行以 `↳` 标记来源，续行缩进对齐，
/// 正文用常规文本色保持可读；空白段落不产生空行。
fn build_agent_message_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for paragraph in text
        .lines()
        .filter(|paragraph| !paragraph.trim().is_empty())
    {
        for line in crate::ui::drawer::wrap_preserving_display_width(
            paragraph,
            content_width.saturating_sub(2).max(1),
        ) {
            let prefix = if lines.is_empty() {
                Span::styled("↳ ", Style::default().fg(crate::ui::theme::ACCENT))
            } else {
                Span::raw("  ")
            };
            lines.push(Line::from(vec![
                prefix,
                Span::styled(line, Style::default().fg(crate::ui::theme::TEXT)),
            ]));
        }
    }
    lines
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
        UiMessage::SystemEvent(UiSystemEvent::AgentMessage(message)) => {
            build_agent_message_lines(&message.text, content_width)
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
                let mut lines = vec![line];
                if task.kind == omini_domain::task::TaskKind::SubAgent
                    && let Some(summary) = &task.summary
                {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "  {}",
                            truncate_display_width(summary, content_width.saturating_sub(2))
                        ),
                        Style::default().fg(crate::ui::theme::MUTED),
                    )));
                }
                if is_completed_agent {
                    lines.push(Line::from(""));
                }
                lines
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
    lookup: &RenderLookup<'_, '_>,
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
            flush_activity_group(activity, None, content_width, all_lines, selectable_lines);
            let result = lookup.result(&tool.id);
            let lines = if tool.name == "send_message" {
                crate::features::tools::agent::render_send_message(
                    tool,
                    result,
                    send_message_target_label(state, tool),
                    content_width,
                )
            } else {
                render_tool(
                    tool,
                    result,
                    None,
                    None,
                    content_width,
                    Some(state.project.status_bar.cwd.as_path()),
                )
            };
            append_rendered_lines(all_lines, selectable_lines, lines);
        }
        BlockView::Text(text) if !text.trim().is_empty() => {
            flush_activity_group(activity, None, content_width, all_lines, selectable_lines);
            append_rendered_lines(
                all_lines,
                selectable_lines,
                build_assistant_text_lines(text, content_width),
            );
        }
        BlockView::Result(result) if !lookup.has_tool(&result.tool_use_id) => {
            activity.orphan_results.push(result.as_ref().clone());
        }
        _ => {}
    }
}

/// 解析 `send_message` 目标 task ID 对应的子任务显示名：优先任务标题，
/// 缺失时回退 Agent 名称；解析不到（如收件方为 `parent`）返回 None，
/// 由渲染回退到工具输入里的原始 target 文本。
/// 发送方拿到 task ID 的前提是 spawn 结果已返回、节点已入列；重连快照也在渲染前
/// 重建节点，因此按消息增量渲染的缓存无需为节点插入登记额外失效。
fn send_message_target_label<'a>(
    state: &'a ViewContext<'_>,
    tool: &omini_model::message::ToolUseBlock,
) -> Option<&'a str> {
    let target = tool.input.get("target")?.as_str()?;
    if let Some(node) = state
        .sessions
        .subagents
        .values()
        .find(|node| node.task_id == target)
    {
        return Some(node.display_title());
    }
    // 子任务结束后节点被回收；沿用回收时记住的标题，避免历史条目退化为原始 task ID。
    state
        .sessions
        .subagent_title_memory
        .get(target)
        .map(String::as_str)
}

/// 仅在会话仍运行且活动组尚未遇到分界时，展示最新一次普通工具调用。
fn render_active_preview(
    state: &ViewContext<'_>,
    activity: &ActivityGroup,
    content_width: usize,
) -> Option<Vec<Line<'static>>> {
    if !state.session.main_query_active && state.session.run_timer.is_none() {
        return None;
    }
    let tool = activity.last_tool()?;
    Some(render_activity_preview(
        tool,
        content_width,
        Some(state.project.status_bar.cwd.as_path()),
    ))
}

fn flush_activity_group(
    activity: &mut ActivityGroup,
    preview: Option<Vec<Line<'static>>>,
    content_width: usize,
    all_lines: &mut Vec<Line<'static>>,
    selectable_lines: &mut Vec<String>,
) {
    if activity.is_empty() {
        return;
    }
    let group = std::mem::take(activity);
    let mut summary = group.summary(content_width);
    if let Some(preview) = preview {
        summary.extend(preview);
    }
    append_rendered_lines(all_lines, selectable_lines, summary);
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
    fn agent_message_uses_quiet_injection_style() {
        // 给定主 Agent 注入子会话的多段消息。
        let lines = build_agent_message_lines("请复查边界情况\n\n如果仍有问题再回报", 40);

        let plain: Vec<String> = lines.iter().map(line_to_plain_text).collect();
        // 空白段落不产生空行，首行箭头标记来源，续行缩进对齐。
        assert_eq!(plain, vec!["↳ 请复查边界情况", "  如果仍有问题再回报"]);
        // 首行箭头使用主题色，正文恢复常规文本色，不再有独立的主 Agent 标题行。
        assert_eq!(lines[0].spans[0].style.fg, Some(crate::ui::theme::ACCENT));
        assert_eq!(lines[1].spans[1].style.fg, Some(crate::ui::theme::TEXT));
    }

    #[test]
    fn task_delivery_summary() {
        // 给定子任务完成通知携带未注入消息的摘要。
        let mut state = AppState::new();
        state.sessions.views["main"]
            .messages
            .push(UiMessage::SystemEvent(UiSystemEvent::TaskNotification(
                omini_domain::conversation::TaskNotification {
                    tasks: vec![omini_domain::task::TaskCompletion {
                        task_id: "task-1".into(),
                        kind: omini_domain::task::TaskKind::SubAgent,
                        label: "Explore".into(),
                        title: "Inspect".into(),
                        status: omini_domain::task::TaskStatus::Cancelled,
                        summary: Some("1 条消息未进入子 Agent 模型上下文".into()),
                    }],
                    created_at: chrono::Utc::now(),
                },
            )));

        // 当主时间线渲染任务完成通知时，摘要应可见。
        let rendered = rendered_timeline(&state);
        assert!(
            rendered.contains("1 条消息未进入子 Agent 模型上下文"),
            "{rendered}"
        );
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

    /// 给定跨消息延续的活动组；当新工具开始及正文出现时，则附件只跟随最后工具并在分界处收起。
    #[test]
    fn tracks_active_preview() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"].main_query_active = true;
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::Thinking(ThinkingBlock {
                        thinking: "hidden".into(),
                        duration_ms: Some(2_000),
                    }),
                    ContentBlock::from_tool_use(
                        "shell-1".into(),
                        "bash".into(),
                        HashMap::from([("command".into(), serde_json::json!("pwd"))]),
                    ),
                ],
            )));

        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        let first = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(
            first.contains("Thought for 2s, ran 1 shell command"),
            "{first}"
        );
        assert!(first.contains("  └ $ pwd"), "{first}");

        state.sessions.views["main"].pending_assistant = Some(
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_tool_use(
                        "search-1".into(),
                        "search".into(),
                        HashMap::from([("query".into(), serde_json::json!("AgentRun"))]),
                    ),
                    ContentBlock::from_tool_result(
                        "search-1".into(),
                        false,
                        "private search output".into(),
                    ),
                ],
            )
            .into(),
        );
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        let latest = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(
            latest.contains("ran 1 shell command, ran 1 search"),
            "{latest}"
        );
        assert!(latest.contains("  └ ⌕ Search  AgentRun in ."), "{latest}");
        assert!(!latest.contains("  └ $ pwd"), "{latest}");
        assert!(!latest.contains("private search output"), "{latest}");

        state.sessions.views["main"]
            .pending_assistant
            .as_mut()
            .unwrap()
            .content
            .push(ContentBlock::from_text("answer".into()));
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        let closed = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(closed.contains("answer"), "{closed}");
        assert!(!closed.contains("  └ ⌕ Search"), "{closed}");
    }

    /// 给定未遇到正文分界的活动组；当运行结束时，则历史只保留摘要。
    #[test]
    fn hides_finished_preview() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"].main_query_active = true;
        state.sessions.views["main"]
            .messages
            .push(assistant_item(thought_and_shell_call(
                2_000, "shell-1", "output",
            )));
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        assert!(
            state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("  └ $ pwd")
        );

        state.sessions.views["main"].main_query_active = false;
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        let finished = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(finished.contains("Thought for 2s, ran 1 shell command"));
        assert!(!finished.contains("  └ $ pwd"), "{finished}");
    }

    /// 给定活跃的普通工具摘要；当独立展示的工具出现时，则先收起摘要附件。
    #[test]
    fn closes_boundary_preview() {
        let mut state = AppState::new();
        state.sessions.views["main"].main_query_active = true;
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_tool_use(
                        "shell-1".into(),
                        "bash".into(),
                        HashMap::from([("command".into(), serde_json::json!("pwd"))]),
                    ),
                    ContentBlock::from_tool_use(
                        "edit-1".into(),
                        "edit".into(),
                        HashMap::from([("file_path".into(), serde_json::json!("src/lib.rs"))]),
                    ),
                ],
            )));

        let rendered = rendered_timeline(&state);
        assert!(rendered.contains("Ran 1 shell command"), "{rendered}");
        assert!(rendered.contains("⏺ Patch"), "{rendered}");
        assert!(!rendered.contains("  └ $ pwd"), "{rendered}");
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
            ContentBlock::from_tool_use(
                "send-1".into(),
                "send_message".into(),
                HashMap::from([
                    ("target".into(), serde_json::json!("task-1")),
                    ("message".into(), serde_json::json!("请复查边界情况")),
                ]),
            ),
            ContentBlock::from_tool_use("run-2".into(), "run_agent".into(), HashMap::new()),
        ];
        let results = ["run-1", "read-1", "cancel-1", "spawn-1", "send-1", "run-2"]
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
        // send_message 按目标 task ID 解析子任务标题，作为分界条目展示。
        assert!(rendered.contains("↪ Search architecture · 请复查边界情况"));
        assert!(!rendered.contains("后台 ·"));
        assert!(!rendered.contains("同步 ·"));
        assert!(!rendered.to_lowercase().contains("ctrl+"));
        assert!(!rendered.contains("to expand"));
        assert!(rendered.contains("Search architecture"));
    }

    /// 给定缓存路径已按子任务标题渲染 send_message 条目；当子任务结束被回收且触发
    /// 宽度变化全量重绘时，则条目仍显示回收时记忆的标题而不是原始 task ID。
    #[test]
    fn send_message_label_survives_task_prune_and_rebuild() {
        let mut state = AppState::new();
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
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
                Role::Assistant,
                vec![ContentBlock::from_tool_use(
                    "send-1".into(),
                    "send_message".into(),
                    HashMap::from([
                        ("target".into(), serde_json::json!("task-1")),
                        ("message".into(), serde_json::json!("请复查边界情况")),
                    ]),
                )],
            )));

        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        terminal
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 80, 16)))
            .unwrap();
        let before = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(
            before.contains("↪ Search architecture · 请复查边界情况"),
            "{before}"
        );

        let child = state.sessions.subagents.get_mut("thread-1").unwrap();
        child.status = omini_domain::task::TaskStatus::Completed;
        state.prune_terminal_tasks();
        // 换一块更宽的终端强制缓存全量重建，走标题记忆而非活动节点。
        let mut resized = Terminal::new(TestBackend::new(100, 16)).unwrap();
        resized
            .draw(|frame| render_messages(&mut state, frame, Rect::new(0, 0, 100, 16)))
            .unwrap();
        let after = state.sessions.views["main"]
            .selectable_message_lines
            .join("\n");
        assert!(
            after.contains("↪ Search architecture · 请复查边界情况"),
            "{after}"
        );
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
            ContentBlock::from_tool_use(
                "send-1".into(),
                "send_message".into(),
                HashMap::from([
                    ("target".into(), serde_json::json!("task-9")),
                    ("message".into(), serde_json::json!("检查输出路径")),
                ]),
            ),
            ContentBlock::from_tool_use("search-2".into(), "search".into(), HashMap::new()),
        ];
        blocks.extend(
            [
                "bash-1", "read-1", "edit-1", "bash-2", "write-1", "search-1", "ask-1", "read-2",
                "todo-1", "bash-3", "spawn-1", "send-1", "search-2",
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
            "↪ task-9 · 检查输出路径",
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
            UiMessage::SystemEvent(UiSystemEvent::AgentMessage(
                omini_domain::conversation::AgentMessage {
                    source_run_id: "main-run".into(),
                    tool_use_id: "tool-1".into(),
                    text: "keep going".into(),
                },
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

    /// 给定历史已经绘制；当输入、滚动与高度变化时，则旧消息不再投影或解析。
    #[test]
    fn verify_cache_hits() {
        let mut state = AppState::new();
        for index in 0..60 {
            state.sessions.views["main"]
                .messages
                .push(assistant_item(Message::new(
                    Role::Assistant,
                    vec![ContentBlock::from_text(format!("**entry {index}**"))],
                )));
        }

        assert_cached_reference(&mut state, 80, 24);
        let first_passes = state.sessions.views["main"]
            .render_cache
            .borrow()
            .history_passes;
        assert_eq!(first_passes, 60);

        state.composer.input = "draft".into();
        state.sessions.views["main"].auto_scroll = false;
        state.sessions.views["main"].scroll_offset = 3;
        assert_cached_reference(&mut state, 80, 36);
        assert_eq!(
            state.sessions.views["main"]
                .render_cache
                .borrow()
                .history_passes,
            first_passes
        );

        state.sessions.views["main"]
            .messages
            .push(user_input_item("next"));
        assert_cached_reference(&mut state, 80, 24);
        assert_eq!(
            state.sessions.views["main"]
                .render_cache
                .borrow()
                .history_passes,
            first_passes + 1
        );

        assert_cached_reference(&mut state, 120, 36);
        assert_eq!(
            state.sessions.views["main"]
                .render_cache
                .borrow()
                .history_passes,
            first_passes + 62
        );
    }

    /// 给定工具结果可能晚到或先于调用；当索引变化时，则只回退受影响后缀。
    #[test]
    fn verify_result_rewind() {
        let mut state = AppState::new();
        for index in 0..20 {
            state.sessions.views["main"]
                .messages
                .push(user_input_item(&format!("prefix {index}")));
        }
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
                Role::Assistant,
                vec![ContentBlock::from_tool_use(
                    "agent-1".into(),
                    "spawn_agent".into(),
                    HashMap::from([("title".into(), serde_json::json!("Inspect"))]),
                )],
            )));
        state.sessions.views["main"]
            .messages
            .push(user_input_item("after agent"));
        assert_cached_reference(&mut state, 100, 24);
        state.sessions.views["main"].pending_assistant = Some(
            Message::new(
                Role::Assistant,
                vec![ContentBlock::from_tool_result(
                    "agent-1".into(),
                    true,
                    "temporary failure".into(),
                )],
            )
            .into(),
        );
        assert_cached_reference(&mut state, 100, 24);
        state.sessions.views["main"].pending_assistant = None;
        assert_cached_reference(&mut state, 100, 24);
        let before = state.sessions.views["main"]
            .render_cache
            .borrow()
            .history_passes;

        state.sessions.views["main"]
            .messages
            .push(UiMessage::SystemEvent(UiSystemEvent::ToolResults {
                results: vec![ToolResultRecord {
                    tool_use_id: "agent-1".into(),
                    is_error: true,
                    content: "agent failed".into(),
                    metadata: None,
                }],
            }));
        assert_cached_reference(&mut state, 100, 24);
        let after = state.sessions.views["main"]
            .render_cache
            .borrow()
            .history_passes;
        assert!(after > before + 1 && after < before + 20);
        assert!(
            state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("agent failed")
        );

        state.sessions.views["main"]
            .messages
            .push(UiMessage::SystemEvent(UiSystemEvent::ToolResults {
                results: vec![ToolResultRecord {
                    tool_use_id: "future".into(),
                    is_error: false,
                    content: "orphan output".into(),
                    metadata: None,
                }],
            }));
        assert_cached_reference(&mut state, 100, 24);
        assert!(
            state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("orphan output")
        );
        state.sessions.views["main"]
            .messages
            .push(assistant_item(Message::new(
                Role::Assistant,
                vec![ContentBlock::from_tool_use(
                    "future".into(),
                    "bash".into(),
                    HashMap::new(),
                )],
            )));
        assert_cached_reference(&mut state, 100, 24);
        assert!(
            !state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("orphan output")
        );
    }

    /// 给定主会话与子会话都已缓存；当切换视图和替换快照时，则不会复用错误历史。
    #[test]
    fn verify_session_isolation() {
        let mut state = AppState::new();
        state.sessions.views["main"]
            .messages
            .push(user_input_item("main"));
        assert_cached_reference(&mut state, 80, 24);
        state.sessions.views.insert(
            "child".into(),
            crate::features::sessions::model::SessionState {
                messages: vec![user_input_item("child")],
                ..Default::default()
            },
        );
        state.sessions.active_session_task_id = Some("child".into());
        assert_cached_reference(&mut state, 80, 24);
        assert_eq!(
            state.sessions.views["main"]
                .render_cache
                .borrow()
                .history_passes,
            1
        );
        assert_eq!(
            state.sessions.views["child"]
                .render_cache
                .borrow()
                .history_passes,
            1
        );

        state.sessions.active_session_task_id = None;
        assert_cached_reference(&mut state, 80, 24);
        assert_eq!(
            state.sessions.views["main"]
                .render_cache
                .borrow()
                .history_passes,
            1
        );
        state.apply_thread_snapshot(
            Some("replacement".into()),
            vec![omini_protocol::HistoryItem::UserInput(
                match user_input_item("new main") {
                    UiMessage::UserInput(input) => input,
                    _ => unreachable!(),
                },
            )],
            Vec::new(),
            ThreadUsageSnapshot::default(),
        );
        assert_cached_reference(&mut state, 80, 24);
        assert!(
            state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("new main")
        );
        assert!(!state.sessions.views.contains_key("child"));
    }

    /// 给定历史已经缓存；当流式尾部增长、提交和分隔线清除时，则逐次保持全量结果。
    #[test]
    fn verify_stream_invalidation() {
        let mut state = AppState::new();
        state.sessions.views["main"]
            .messages
            .push(UiMessage::SystemEvent(UiSystemEvent::UserInputEcho(
                crate::features::timeline::model::UserDraft::plain("hello".into()),
            )));
        assert_cached_reference(&mut state, 80, 24);
        state.apply_event(RuntimeToUiEvent::UserMessageInjected {
            item: omini_protocol::HistoryItem::UserInput(match user_input_item("hello") {
                UiMessage::UserInput(input) => input,
                _ => unreachable!(),
            }),
            client_echo_id: None,
        });
        assert_cached_reference(&mut state, 80, 24);
        let committed = state.sessions.views["main"]
            .render_cache
            .borrow()
            .history_passes;
        assert_eq!(committed, 2);

        state.apply_event(RuntimeToUiEvent::TextDelta("stream".into()));
        assert_cached_reference(&mut state, 80, 24);
        state.apply_event(RuntimeToUiEvent::TextDelta(" continues".into()));
        assert_cached_reference(&mut state, 80, 24);
        assert_eq!(
            state.sessions.views["main"]
                .render_cache
                .borrow()
                .history_passes,
            committed
        );

        state.apply_event(RuntimeToUiEvent::TurnEnded);
        assert_cached_reference(&mut state, 80, 24);
        state.start_run_timer();
        state.apply_event(RuntimeToUiEvent::RunFinished);
        assert_cached_reference(&mut state, 80, 24);
        assert!(
            state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("Worked for")
        );
        state.apply_event(RuntimeToUiEvent::RunStarted);
        assert_cached_reference(&mut state, 80, 24);
        assert!(
            !state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("Worked for")
        );
    }

    /// 给定已显示的子任务通知；当运行事实补齐时，则只重建通知及其后缀。
    #[test]
    fn verify_notice_rewind() {
        use omini_domain::task::{
            TaskChangedEvent, TaskCompletion, TaskInfo, TaskKind, TaskStatus,
        };
        let mut state = AppState::new();
        for index in 0..12 {
            state.sessions.views["main"]
                .messages
                .push(user_input_item(&format!("prefix {index}")));
        }
        state.sessions.views["main"]
            .messages
            .push(UiMessage::SystemEvent(UiSystemEvent::TaskNotification(
                omini_domain::conversation::TaskNotification {
                    tasks: vec![TaskCompletion {
                        task_id: "task-1".into(),
                        kind: TaskKind::SubAgent,
                        label: "Explore".into(),
                        title: "Inspect".into(),
                        status: TaskStatus::Completed,
                        summary: None,
                    }],
                    created_at: chrono::Utc::now(),
                },
            )));
        assert_cached_reference(&mut state, 80, 24);
        let before = state.sessions.views["main"]
            .render_cache
            .borrow()
            .history_passes;
        let now = chrono::Utc::now();
        state.sessions.subagents.insert(
            "thread-1".into(),
            crate::app::state::SubagentNode {
                task_id: "task-1".into(),
                thread_id: "thread-1".into(),
                parent_thread_id: "parent".into(),
                spawn_tool_use_id: "spawn-1".into(),
                agent_label: "Explore".into(),
                title: "Inspect".into(),
                execution_mode: AgentTaskExecutionMode::Background,
                status: TaskStatus::Running,
                duration: None,
                started_at: now - chrono::Duration::seconds(67),
                messages: Vec::new(),
            },
        );
        state.sessions.subagent_order.push("task-1".into());
        state.apply_event(RuntimeToUiEvent::TaskChanged(TaskChangedEvent {
            task: TaskInfo {
                task_id: "task-1".into(),
                owner_thread_id: "parent".into(),
                kind: TaskKind::SubAgent,
                title: "Inspect".into(),
                status: TaskStatus::Completed,
                created_at: now - chrono::Duration::seconds(67),
                updated_at: now,
                completed_at: Some(now),
                result_summary: None,
            },
        }));
        assert_cached_reference(&mut state, 80, 24);
        assert_eq!(
            state.sessions.views["main"]
                .render_cache
                .borrow()
                .history_passes,
            before + 1
        );
        assert!(
            state.sessions.views["main"]
                .selectable_message_lines
                .join("\n")
                .contains("1m 7s")
        );
    }

    fn assert_cached_reference(state: &mut AppState, width: u16, height: u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_messages(state, frame, Rect::new(0, 0, width, height)))
            .unwrap();
        let (lines, selectable) = render_message_range(state, width as usize);
        assert_eq!(state.sessions.active().selectable_message_lines, selectable);

        let mut expected = Terminal::new(TestBackend::new(width, height)).unwrap();
        expected
            .draw(|frame| {
                let context = SectionRenderContext {
                    scroll_y: state.sessions.active().message_scroll_y,
                    visible_height: height as usize,
                    area: Rect::new(0, 0, width, height),
                    user_bg: USER_MESSAGE_BG,
                    user_line_bg: Style::default()
                        .fg(crate::ui::theme::TEXT)
                        .bg(USER_MESSAGE_BG),
                };
                render_line_section(&lines, 0, &context, frame.buffer_mut());
            })
            .unwrap();
        assert_eq!(terminal.backend().buffer(), expected.backend().buffer());
    }
}
