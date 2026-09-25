use super::{build_assistant_text_lines, line_to_plain_text};
use crate::state::{UiMessage, UiState};
use crate::widgets::{ToolCategory, activity_summary_line, is_special_tool, tool_category};
use omini_domain::message::{ContentBlock, Message, Role};
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub(super) fn trailing_activity_group_start(messages: &[UiMessage]) -> Option<usize> {
    let mut start = messages.len();
    let mut has_assistant_activity = false;
    for (index, ui_message) in messages.iter().enumerate().rev() {
        let UiMessage::Message(message) = ui_message else {
            break;
        };
        if !is_activity_message(messages, index, message) {
            break;
        }
        has_assistant_activity |= message.role == Role::Assistant;
        start = index;
    }
    (start < messages.len() && has_assistant_activity).then_some(start)
}

pub(super) fn activity_group_end(
    messages: &[UiMessage],
    start: usize,
    end: usize,
) -> Option<usize> {
    let UiMessage::Message(first) = messages.get(start)? else {
        return None;
    };
    if first.role != Role::Assistant || !is_assistant_activity_message(first) {
        return None;
    }

    let mut group_end = start + 1;
    while group_end < end {
        let UiMessage::Message(next) = &messages[group_end] else {
            break;
        };
        if !is_activity_message(messages, group_end, next) {
            break;
        }
        group_end += 1;
        if is_ask_user_message(next) {
            break;
        }
    }
    Some(group_end)
}

fn is_activity_message(messages: &[UiMessage], index: usize, message: &Message) -> bool {
    match message.role {
        Role::Assistant => is_assistant_activity_message(message),
        // 已持久化的工具结果使用 user 角色。只有调用属于普通工具时才延续当前活动，ask_user 回复除外。
        Role::User => message.content.iter().all(|block| {
            let ContentBlock::ToolResult(result) = block else {
                return false;
            };
            messages[..index]
                .iter()
                .filter_map(UiMessage::as_message)
                .flat_map(|prior| &prior.content)
                .any(|prior_block| {
                    matches!(prior_block, ContentBlock::ToolUse(tool_use)
                        if tool_use.id == result.tool_use_id && !is_special_tool(tool_use))
                })
        }),
    }
}

fn is_assistant_activity_message(message: &Message) -> bool {
    if message.content.is_empty() {
        return false;
    }

    let has_regular_tool_use = message.content.iter().any(
        |block| matches!(block, ContentBlock::ToolUse(tool_use) if !is_special_tool(tool_use)),
    );
    let has_visible_text = message.content.iter().any(|block| {
        matches!(block, ContentBlock::Text(text)
            // 这里只检查文本是否可见；Markdown 排版会按给定宽度分配行，使用无界宽度可能导致溢出。
            if build_assistant_text_lines(&text.text, 80)
                .iter()
                .any(|line| !line_to_plain_text(line).trim().is_empty()))
    });

    // 工具说明和待处理的 ask_user 提示可能与 Thought 共处于一条 assistant 消息中；保留其文本和工具界面，
    // 同时将 Thought 合并到周围的活动摘要中。
    (!has_visible_text || has_regular_tool_use || is_ask_user_message(message))
        && message.content.iter().all(|block| match block {
            ContentBlock::Thinking(_) | ContentBlock::ToolResult(_) | ContentBlock::Image(_) => {
                true
            }
            ContentBlock::ToolUse(tool_use) => {
                !is_special_tool(tool_use) || tool_use.name == "ask_user"
            }
            ContentBlock::Text(_) => true,
        })
}

fn is_ask_user_message(message: &Message) -> bool {
    message.role == Role::Assistant
        && message.content.iter().any(
            |block| matches!(block, ContentBlock::ToolUse(tool_use) if tool_use.name == "ask_user"),
        )
}

pub(super) fn render_pending_activity_group(
    state: &UiState,
    group_start: usize,
    pending_prefix_len: usize,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let Some(pending) = state.pending_assistant.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let mut rendered_messages: Vec<&Message> = state
        .messages
        .iter()
        .filter_map(UiMessage::as_message)
        .collect();
    rendered_messages.push(pending);

    let tool_result_map = tool_result_index(&rendered_messages);
    let pending_prefix = Message::new(
        omini_domain::message::Role::Assistant,
        pending.content[..pending_prefix_len].to_vec(),
    );
    let mut activity_messages = state.messages[group_start..]
        .iter()
        .filter_map(UiMessage::as_message)
        .collect::<Vec<_>>();
    activity_messages.push(&pending_prefix);
    let mut consumed = HashSet::new();
    let lines = render_activity_summary(
        &activity_messages,
        &tool_result_map,
        &mut consumed,
        content_width,
        state.thinking_started_at.map(|started| started.elapsed()),
    );
    let selectable = lines.iter().map(line_to_plain_text).collect();
    (lines, selectable)
}

pub(super) fn tool_result_index(messages: &[&Message]) -> HashMap<String, Vec<(usize, usize)>> {
    let mut index = HashMap::new();
    for (message_idx, message) in messages.iter().enumerate() {
        for (block_idx, block) in message.content.iter().enumerate() {
            if let ContentBlock::ToolResult(result) = block {
                index
                    .entry(result.tool_use_id.clone())
                    .or_insert_with(Vec::new)
                    .push((message_idx, block_idx));
            }
        }
    }
    index
}

pub(super) fn render_activity_summary(
    messages: &[&Message],
    tool_result_map: &HashMap<String, Vec<(usize, usize)>>,
    consumed: &mut HashSet<(usize, usize)>,
    content_width: usize,
    active_thinking: Option<Duration>,
) -> Vec<Line<'static>> {
    let mut thinking_ms: Option<u64> = None;
    for message in messages {
        for block in &message.content {
            if let ContentBlock::Thinking(thinking) = block
                && let Some(ms) = thinking.duration_ms
            {
                thinking_ms = Some(thinking_ms.unwrap_or(0).saturating_add(ms));
            }
        }
    }
    if let Some(duration) = active_thinking {
        let elapsed = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        thinking_ms = Some(thinking_ms.unwrap_or(0).saturating_add(elapsed));
    }

    let mut tool_counts: Vec<(ToolCategory, usize)> = Vec::new();
    for message in messages {
        for block in &message.content {
            let ContentBlock::ToolUse(tool_use) = block else {
                continue;
            };
            if is_special_tool(tool_use) {
                continue;
            }
            let category = tool_category(tool_use);
            if let Some((_, count)) = tool_counts.iter_mut().find(|(item, _)| *item == category) {
                *count += 1;
            } else {
                tool_counts.push((category.clone(), 1));
            }

            if let Some(positions) = tool_result_map.get(&tool_use.id) {
                consumed.extend(positions.iter().copied());
            }
        }
    }

    let Some(summary) = activity_summary_line(thinking_ms, &tool_counts, content_width) else {
        return Vec::new();
    };
    vec![summary]
}
