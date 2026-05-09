use crate::runtime::AgentRuntime;
use crate::types::config::Settings;
use crate::types::events::{RuntimeEvent, UiRequest};
use crate::types::message::{ContentBlock, Message, Role, ToolResultBlock, ToolUseBlock};
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::{HashMap, HashSet};
use std::io::{self, stderr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default, PartialEq)]
pub enum AgentStatus {
    #[default]
    Idle,
    /// LLM 思考中
    Thinking,
    /// 工具执行中
    Working,
    /// 等待用户操作（权限确认/回答问题）
    AwaitingInput,
    Error(String),
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Idle => write!(f, "● Ready"),
            AgentStatus::Thinking => write!(f, "○ Thinking…"),
            AgentStatus::Working => write!(f, "● Working…"),
            AgentStatus::AwaitingInput => write!(f, "◐ Waiting for you"),
            AgentStatus::Error(e) => write!(f, "✕ {e}"),
        }
    }
}

#[derive(Debug)]
pub struct UiState {
    pub messages: Vec<Message>,
    /// 正在流式构建中的 assistant 消息（SSE 实时显示）
    pub pending_assistant: Option<Message>,
    /// 渲染后的消息总行数（用于滚动条计算）
    pub total_lines: usize,
    /// 消息区域的位置和大小
    pub messages_area: Rect,
    pub input: String,
    /// 光标偏移量，按 Unicode 字符计数（不是字节）
    pub cursor_char: usize,
    pub agent_status: AgentStatus,
    /// 从底部向上滚动的行数（0 = 位于底部，显示最新消息）
    pub scroll_offset: usize,
    /// 自适应滚动步长（根据滚动速度动态调整）
    pub scroll_step: usize,
    /// 上次滚动时间戳（用于速度计算）
    pub last_scroll_time: Option<tokio::time::Instant>,
    /// Agent runtime 的 JoinHandle，用于生命周期管理
    pub runtime_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

impl UiState {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            pending_assistant: None,
            total_lines: 0,
            messages_area: Rect::default(),
            input: String::new(),
            cursor_char: 0,
            agent_status: AgentStatus::Idle,
            scroll_offset: 0,
            scroll_step: 1,
            last_scroll_time: None,
            runtime_handle: None,
        }
    }

    pub fn apply_event(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::RunStarted => {
                self.pending_assistant = None;
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeEvent::TurnStarted => {
                // 如果上轮还有未提交的 pending_assistant，先推入 messages
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(msg);
                }
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeEvent::ThinkingDelta(t) => {
                self.agent_status = AgentStatus::Thinking;
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                if let Some(ContentBlock::Thinking(tb)) = pending.content.last_mut() {
                    tb.thinking.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_thinking(t));
                }
            }
            RuntimeEvent::TextDelta(t) => {
                self.agent_status = AgentStatus::Working;
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                if let Some(ContentBlock::Text(tb)) = pending.content.last_mut() {
                    tb.text.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_text(t));
                }
            }
            RuntimeEvent::ToolUse(tu) => {
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                pending.content.push(ContentBlock::ToolUse(tu));
                self.agent_status = AgentStatus::Working;
            }
            RuntimeEvent::ToolResult(tr) => {
                // 工具结果异步返回，追加到 pending_assistant 或最后一条消息中
                if let Some(pending) = &mut self.pending_assistant {
                    pending.content.push(ContentBlock::ToolResult(tr));
                } else if let Some(last) = self.messages.last_mut() {
                    last.content.push(ContentBlock::ToolResult(tr));
                } else {
                    let mut msg = Message::new(Role::Assistant, Vec::new());
                    msg.content.push(ContentBlock::ToolResult(tr));
                    self.messages.push(msg);
                }
            }
            RuntimeEvent::TurnEnded => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(msg);
                }
                self.scroll_offset = 0;
                self.agent_status = AgentStatus::Working;
            }
            RuntimeEvent::RunFinished => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(msg);
                }
                self.scroll_offset = 0;
                self.agent_status = AgentStatus::Idle;
            }
            RuntimeEvent::PermissionRequest(_) => {
                // TODO: 实现权限请求弹窗 UI
                self.agent_status = AgentStatus::AwaitingInput;
            }
            RuntimeEvent::UserConfirmation(_) => {
                // TODO: 实现用户确认输入 UI
                self.agent_status = AgentStatus::AwaitingInput;
            }
            RuntimeEvent::Error(e) => self.agent_status = AgentStatus::Error(e),
        }
    }

    // ── 输入编辑 ──

    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.input.chars().take(char_idx).map(char::len_utf8).sum()
    }

    fn insert_char(&mut self, c: char) {
        let byte_idx = self.char_to_byte(self.cursor_char);
        self.input.insert(byte_idx, c);
        self.cursor_char += 1;
    }

    fn delete_before(&mut self) {
        if self.cursor_char > 0 {
            self.cursor_char -= 1;
            let byte_idx = self.char_to_byte(self.cursor_char);
            self.input.remove(byte_idx);
        }
    }

    fn delete_after(&mut self) {
        let byte_idx = self.char_to_byte(self.cursor_char);
        if byte_idx < self.input.len() {
            self.input.remove(byte_idx);
        }
    }

    fn cursor_left(&mut self) {
        self.cursor_char = self.cursor_char.saturating_sub(1);
    }

    fn cursor_right(&mut self) {
        let max_chars = self.input.chars().count();
        if self.cursor_char < max_chars {
            self.cursor_char += 1;
        }
    }

    fn cursor_home(&mut self) {
        self.cursor_char = 0;
    }

    fn cursor_end(&mut self) {
        self.cursor_char = self.input.chars().count();
    }

    // ── 滚动 ──

    /// 根据滚动速度动态调整步长
    pub fn update_scroll_step(&mut self, now: tokio::time::Instant) {
        const MIN_STEP: usize = 1;
        const MAX_STEP: usize = 10;
        const ACCEL_MS: u64 = 80; // 间隔 < 80ms → 加速
        const DECEL_MS: u64 = 250; // 间隔 > 250ms → 减速
        const RESET_MS: u64 = 800; // 间隔 > 800ms → 重置为初始值

        if let Some(last) = self.last_scroll_time {
            let elapsed = now.saturating_duration_since(last);
            let ms = elapsed.as_millis() as u64;

            if ms > RESET_MS {
                self.scroll_step = MIN_STEP;
            } else if ms < ACCEL_MS {
                self.scroll_step = (self.scroll_step + 1).min(MAX_STEP);
            } else if ms > DECEL_MS {
                self.scroll_step = (self.scroll_step / 2).max(MIN_STEP);
            }
            // 中间区间：保持当前步长
        } else {
            self.scroll_step = MIN_STEP;
        }
        self.last_scroll_time = Some(now);
    }

    fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }
}

fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return text.lines().map(|l| l.to_string()).collect();
    }

    let mut result = Vec::new();

    for line in text.split('\n') {
        let line_width = UnicodeWidthStr::width(line);
        if line_width <= max_width {
            result.push(line.to_string());
            continue;
        }

        let mut start = 0;
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();

        while start < len {
            let mut end = start;
            let mut w = 0;
            while end < len {
                let cw = UnicodeWidthChar::width(chars[end]).unwrap_or(0);
                if w + cw > max_width {
                    break;
                }
                w += cw;
                end += 1;
            }

            if end == start {
                end = start + 1;
            } else if end < len && !chars[end].is_whitespace() {
                let mut break_at = end;
                while break_at > start && !chars[break_at - 1].is_whitespace() {
                    break_at -= 1;
                }
                if break_at > start {
                    end = break_at;
                }
            }

            let segment: String = chars[start..end].iter().collect();
            result.push(segment.trim_end().to_string());

            start = end;
            while start < len && chars[start].is_whitespace() {
                start += 1;
            }
        }
    }

    result
}

fn build_bordered_lines(
    text: &str,
    content_width: usize,
    border_color: Color,
    italic: bool,
    bg: Option<Color>,
) -> Vec<Line<'static>> {
    let available = content_width.saturating_sub(2);

    let mut lines: Vec<Line> = Vec::new();

    let mut content_style = if italic {
        Style::default()
            .fg(border_color)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default()
    };
    if let Some(c) = bg {
        content_style = content_style.bg(c);
    }

    let border_span = |c: Color| -> Span<'static> {
        let mut style = Style::default().fg(c);
        if let Some(bg_c) = bg {
            style = style.bg(bg_c);
        }
        Span::styled("\u{2503}", style)
    };

    let space_span = || -> Span<'static> {
        if let Some(c) = bg {
            Span::raw(" ").style(Style::default().bg(c))
        } else {
            Span::raw(" ")
        }
    };

    if text.is_empty() {
        lines.push(Line::from(vec![border_span(border_color)]));
    } else {
        let wrapped = word_wrap(text, available);
        for wrapped_line in wrapped {
            lines.push(Line::from(vec![
                border_span(border_color),
                space_span(),
                Span::styled(wrapped_line, content_style),
            ]));
        }
    }

    lines
}

fn build_plain_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let wrapped = word_wrap(text, content_width);
    wrapped.into_iter().map(Line::from).collect()
}

fn build_thinking_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    let available = content_width.saturating_sub(2);
    let mut lines: Vec<Line> = Vec::new();

    let border_style = Style::default().fg(Color::DarkGray);
    let prefix_style = Style::default()
        .fg(Color::Rgb(141, 119, 78))
        .add_modifier(Modifier::ITALIC);
    let text_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC);

    if text.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("\u{2503}", border_style),
            Span::raw(" "),
            Span::styled("Thinking: ", prefix_style),
        ]));
        return lines;
    }

    let prefix = "Thinking: ";
    let prefix_w = UnicodeWidthStr::width(prefix);
    let wrapped = word_wrap(text, available);

    for (i, wl) in wrapped.iter().enumerate() {
        if i == 0 {
            let wl_w = UnicodeWidthStr::width(wl.as_str());
            if prefix_w + wl_w <= available {
                // "Thinking: <text>" fits on first line
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(prefix, prefix_style),
                    Span::styled(wl.clone(), text_style),
                ]));
            } else {
                // "Thinking: " on its own line, text on next line(s)
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(prefix, prefix_style),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("\u{2503}", border_style),
                    Span::raw(" "),
                    Span::styled(wl.clone(), text_style),
                ]));
            }
        } else {
            lines.push(Line::from(vec![
                Span::styled("\u{2503}", border_style),
                Span::raw(" "),
                Span::styled(wl.clone(), text_style),
            ]));
        }
    }

    lines
}

fn build_tool_group_lines(
    tool_use: &ToolUseBlock,
    tool_result: &ToolResultBlock,
    content_width: usize,
) -> Vec<Line<'static>> {
    let available = content_width.saturating_sub(2);
    let mut lines: Vec<Line> = Vec::new();

    let tool_color = Color::Yellow;
    let result_color = if tool_result.is_error {
        Color::Red
    } else {
        Color::Green
    };

    // ╭─ Tool: create_file ─────────────────
    let tool_header = format!(" Tool: {} ", tool_use.name);
    let tool_header_width = UnicodeWidthStr::width(&*tool_header);
    let dashes = "─".repeat(available.saturating_sub(tool_header_width));
    let top_line = format!("╭─{}{}", tool_header, dashes);
    lines.push(Line::from(Span::styled(
        top_line,
        Style::default().fg(tool_color),
    )));

    // Tool input content
    let input_text = serde_json::to_string_pretty(&tool_use.input).unwrap_or_default();
    if input_text.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("│{}", " ".repeat(available + 1)),
            Style::default().fg(tool_color),
        )));
    } else {
        for wl in word_wrap(&input_text, available) {
            lines.push(Line::from(Span::styled(
                format!("│ {}", wl),
                Style::default().fg(tool_color),
            )));
        }
    }

    // ├─ Result (success) ─────────────────
    let status = if tool_result.is_error {
        " Error "
    } else {
        " Success "
    };
    let status_width = UnicodeWidthStr::width(status);
    let sep_dashes = "─".repeat(available.saturating_sub(status_width));
    let sep_line = format!("├─{}{}", status, sep_dashes);
    lines.push(Line::from(Span::styled(
        sep_line,
        Style::default().fg(result_color),
    )));

    // Tool result content
    if tool_result.output.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("│{}", " ".repeat(available + 1)),
            Style::default().fg(result_color),
        )));
    } else {
        for wl in word_wrap(&tool_result.output, available) {
            lines.push(Line::from(Span::styled(
                format!("│ {}", wl),
                Style::default().fg(result_color),
            )));
        }
    }

    // ╰─────────────────────────────────────
    let bottom_line = format!("╰{}", "─".repeat(content_width - 1));
    lines.push(Line::from(Span::styled(
        bottom_line,
        Style::default().fg(result_color),
    )));

    lines
}

// ── 渲染 ───────────────────────────────────────────────────────────

fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    let area = frame.area();

    let chunks = Layout::vertical([
        Constraint::Length(1), // Header
        Constraint::Length(1), // 分割header和main区域的空行
        Constraint::Min(1),    // Messages
        Constraint::Length(1), // 分割main和input区域的空行
        Constraint::Length(3), // Input
        Constraint::Length(1), // Footer
    ])
    .split(area);
    state.messages_area = chunks[2];

    render_header(&*state, frame, chunks[0]);
    render_messages(state, frame, chunks[2]);
    render_input(&*state, frame, chunks[4]);
    render_footer(&*state, frame, chunks[5]);
}

fn render_header(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let status_color = match state.agent_status {
        AgentStatus::Idle => Color::Green,
        AgentStatus::Thinking => Color::Yellow,
        AgentStatus::Working => Color::Yellow,
        AgentStatus::AwaitingInput => Color::Cyan,
        AgentStatus::Error(_) => Color::Red,
    };

    let line = Line::from(vec![
        Span::styled(" omini ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("v0.1.0  "),
        Span::styled(
            state.agent_status.to_string(),
            Style::default().fg(status_color),
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

    let mut all_lines: Vec<Line> = Vec::new();

    // Build tool result lookup: tool_use_id → Vec<(message_idx, block_idx)>
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
            // Skip ToolResult blocks already rendered as part of a group
            if let ContentBlock::ToolResult(_) = block
                && consumed.contains(&(msg_idx, block_idx))
            {
                continue;
            }

            // Blank line between display items
            if !all_lines.is_empty() {
                all_lines.push(Line::from(""));
            }

            match block {
                ContentBlock::Text(tb) if message.role == Role::User => {
                    // User 消息 → 与输入框完全一致：❯ 前缀 + 背景色
                    let user_bg = Color::Rgb(65, 69, 76);
                    let bg_style = Style::default().bg(user_bg);
                    // 上边距（占1高度，全宽背景）
                    all_lines.push(Line::from(Span::styled(
                        " ".repeat(content_width),
                        bg_style,
                    )));

                    // 内容行（带 ❯ 前缀，全宽背景）
                    let wrapped = word_wrap(&tb.text, content_width.saturating_sub(2));
                    if wrapped.is_empty() {
                        let text = format!("❯ {}", " ".repeat(content_width.saturating_sub(2)));
                        all_lines.push(Line::from(Span::styled(text, bg_style)));
                    } else {
                        for (idx, wl) in wrapped.iter().enumerate() {
                            let prefix = if idx == 0 { "❯ " } else { "  " };
                            let text = format!("{}{}", prefix, wl);
                            let text_width = UnicodeWidthStr::width(&*text);
                            let remaining = content_width.saturating_sub(text_width);
                            let full_line = format!("{}{}", text, " ".repeat(remaining));
                            all_lines.push(Line::from(Span::styled(full_line, bg_style)));
                        }
                    }

                    // 下边距（占1高度，全宽背景）
                    all_lines.push(Line::from(Span::styled(
                        " ".repeat(content_width),
                        bg_style,
                    )));
                }
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    all_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    // Look for a matching ToolResult
                    if let Some(positions) = tool_result_map.get(&tu.id)
                        && let Some(&(rmi, rbi)) = positions.first()
                        && !consumed.contains(&(rmi, rbi))
                        && let ContentBlock::ToolResult(tr) = &state.messages[rmi].content[rbi]
                    {
                        let mut lines = build_tool_group_lines(tu, tr, content_width);
                        all_lines.append(&mut lines);
                        consumed.insert((rmi, rbi));
                        continue;
                    }
                    // No matching ToolResult → render ToolUse alone
                    let input_text = serde_json::to_string_pretty(&tu.input).unwrap_or_default();
                    let mut lines = build_bordered_lines(
                        &input_text,
                        content_width,
                        Color::Yellow,
                        false,
                        None,
                    );
                    all_lines.append(&mut lines);
                }
                ContentBlock::ToolResult(tr) => {
                    // Unmatched ToolResult → render alone
                    let color = if tr.is_error {
                        Color::Red
                    } else {
                        Color::Green
                    };
                    let mut lines =
                        build_bordered_lines(&tr.output, content_width, color, false, None);
                    all_lines.append(&mut lines);
                }
                ContentBlock::Thinking(th) => {
                    let mut lines = build_thinking_lines(&th.thinking, content_width);
                    all_lines.append(&mut lines);
                }
            }
        }
    }

    // 渲染流式构建中的 assistant 消息
    if let Some(ref pending_msg) = state.pending_assistant {
        for block in &pending_msg.content {
            if !all_lines.is_empty() {
                all_lines.push(Line::from(""));
            }
            match block {
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    all_lines.append(&mut lines);
                }
                ContentBlock::Thinking(th) => {
                    let mut lines = build_thinking_lines(&th.thinking, content_width);
                    all_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    let input_text = serde_json::to_string_pretty(&tu.input).unwrap_or_default();
                    let mut lines = build_bordered_lines(
                        &input_text,
                        content_width,
                        Color::Yellow,
                        false,
                        None,
                    );
                    all_lines.append(&mut lines);
                }
                _ => {}
            }
        }
    }

    let total_lines = all_lines.len();
    state.total_lines = total_lines;
    if total_lines == 0 {
        return;
    }

    let max_scroll = total_lines.saturating_sub(visible_height);
    let capped_offset = state.scroll_offset.min(max_scroll);
    // 写回有效范围，避免 scroll_offset 无限膨胀后需多次滚动才能恢复
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

    // 先铺满整行背景
    let line_bg = Paragraph::new(Line::from(Span::styled(
        " ".repeat(area.width as usize),
        bg,
    )))
    .style(bg);
    frame.render_widget(line_bg, input_line);

    // 再叠文字
    let content = if state.input.is_empty() {
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(Color::DarkGray)),
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

fn render_footer(_state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" Model: claude-sonnet-4 ", Style::default().fg(Color::Gray)),
        Span::styled("\u{2502}", Style::default().fg(Color::DarkGray)),
        Span::styled(" Thinking: off ", Style::default().fg(Color::Gray)),
        Span::styled("\u{2502}", Style::default().fg(Color::DarkGray)),
        Span::styled(" Tokens: -- ", Style::default().fg(Color::Gray)),
    ]);

    let paragraph = Paragraph::new(line).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}

// ── 终端初始化 ──────────────────────────────────────────────────────

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    execute!(stderr(), EnterAlternateScreen)?;
    execute!(stderr(), EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stderr()))
}

fn safe_restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), LeaveAlternateScreen);
    let _ = execute!(io::stderr(), DisableMouseCapture);
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

pub async fn run_ui(settings: Settings) -> io::Result<()> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        safe_restore_terminal();
        prev_hook(panic);
    }));

    let mut terminal = init_terminal()?;
    let mut state = UiState::new();

    let running = Arc::new(AtomicBool::new(true));
    let thread_running = running.clone();

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
    let input_handle = tokio::task::spawn_blocking(move || {
        while thread_running.load(Ordering::Relaxed) {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                match event::read() {
                    Ok(event) => {
                        if input_tx.send(event).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });

    // 创建 Runtime → UI 的事件通道
    let (agent_tx, mut agent_rx) = mpsc::channel::<RuntimeEvent>(256);

    // 创建 UI → Runtime 的请求通道
    let (request_tx, request_rx) = mpsc::channel::<UiRequest>(256);

    // 创建并启动 AgentRuntime
    let runtime = AgentRuntime::new(agent_tx.clone(), request_rx, settings);
    state.runtime_handle = Some(runtime.run());

    terminal.draw(|frame| render(&mut state, frame))?;

    let tick_rate = std::time::Duration::from_millis(50);
    let mut last_tick = tokio::time::Instant::now();

    let result = loop {
        tokio::select! {
            Some(event) = input_rx.recv() => {
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        let page_amt = 1.max(
                            state.messages_area.height as usize / 2
                        );
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => break Ok(()),
                            (KeyCode::Char('\x03'), _) => break Ok(()),
                            (KeyCode::Up, _) => state.scroll_up(1),
                            (KeyCode::Down, _) => state.scroll_down(1),
                            (KeyCode::PageUp, _) => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_up(state.scroll_step.max(page_amt));
                            }
                            (KeyCode::PageDown, _) => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_down(state.scroll_step.max(page_amt));
                            }
                            (KeyCode::Enter, _) => {
                                let msg = std::mem::take(&mut state.input);
                                state.cursor_char = 0;
                                if !msg.is_empty() {
                                    state.messages.push(Message::from_user_text(msg.clone()));
                                    state.scroll_offset = 0;
                                    state.agent_status = AgentStatus::Working;

                                    // 提交到 runtime（runtime 会自己维护对话历史）
                                    let _ = request_tx.send(UiRequest::SendMessage(msg)).await;
                                }
                            }
                            (KeyCode::Backspace, _) => state.delete_before(),
                            (KeyCode::Delete, _) => state.delete_after(),
                            (KeyCode::Char(c), _) => state.insert_char(c),
                            (KeyCode::Left, _) => state.cursor_left(),
                            (KeyCode::Right, _) => state.cursor_right(),
                            (KeyCode::Home, _) => state.cursor_home(),
                            (KeyCode::End, _) => state.cursor_end(),
                            _ => {}
                        }
                    }
                    Event::Resize(_, _) => {}
                    Event::Mouse(mouse) => {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_up(state.scroll_step);
                            }
                            MouseEventKind::ScrollDown => {
                                state.update_scroll_step(tokio::time::Instant::now());
                               state.scroll_down(state.scroll_step);
                          }
                           _ => {}
                        }
                    }
                    _ => {}
                }
                // 事件处理后立即重绘
                last_tick = tokio::time::Instant::now();
                terminal.draw(|frame| render(&mut state, frame))?;
            }

            Some(agent_evt) = agent_rx.recv() => {
                state.apply_event(agent_evt);
            }

            _ = tokio::time::sleep_until(last_tick + tick_rate) => {
                while let Ok(evt) = agent_rx.try_recv() {
                    state.apply_event(evt);
                }
                last_tick = tokio::time::Instant::now();
                terminal.draw(|frame| render(&mut state, frame))?;
            }
        }
    };

    running.store(false, Ordering::Relaxed);
    // 关闭 runtime（如果有正在执行的任务则 abort）
    if let Some(handle) = state.runtime_handle.take() {
        handle.abort();
    }
    let _ = input_handle.await;
    restore_terminal(&mut terminal)?;
    let _ = std::panic::take_hook();
    result
}
