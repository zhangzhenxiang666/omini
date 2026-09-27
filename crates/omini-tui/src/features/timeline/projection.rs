use crate::app::state::{UiMessage, UiSystemEvent};
use crate::features::sessions::model::SessionState;
use omini_domain::conversation::{AssistantMessageBlock, ToolResultRecord};
use omini_model::message::{ContentBlock, ToolResultBlock, ToolUseBlock};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// 投影只借用领域正文。工具参数在组件边界适配，不重建 Provider 消息。
pub enum BlockView<'a> {
    Thinking(Option<u64>),
    Text(&'a str),
    ToolUse(Cow<'a, ToolUseBlock>),
    Result(Cow<'a, ToolResultBlock>),
}

pub enum TimelineEntry<'a> {
    Blocks(Vec<BlockView<'a>>, bool),
    Boundary(&'a UiMessage),
}

pub struct TimelineProjection<'a> {
    pub entries: Vec<TimelineEntry<'a>>,
    pub results: HashMap<String, ToolResultBlock>,
    pub tool_ids: HashSet<String>,
}

impl<'a> TimelineProjection<'a> {
    pub fn new(session: &'a SessionState) -> Self {
        let mut entries = session
            .messages
            .iter()
            .map(project_message)
            .collect::<Vec<_>>();
        if let Some(pending) = &session.pending_assistant {
            entries.push(project_pending(pending));
        }
        let mut results = HashMap::new();
        let mut tool_ids = HashSet::new();
        for entry in &entries {
            if let TimelineEntry::Blocks(blocks, _) = entry {
                for block in blocks {
                    match block {
                        BlockView::ToolUse(tool) => {
                            tool_ids.insert(tool.id.clone());
                        }
                        BlockView::Result(result) => {
                            results
                                .entry(result.tool_use_id.clone())
                                .or_insert_with(|| result.as_ref().clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        Self {
            entries,
            results,
            tool_ids,
        }
    }
}

/// 单条消息的投影供全量校验与增量缓存共用，避免两条渲染路径产生不同的分组语义。
pub fn project_message(message: &UiMessage) -> TimelineEntry<'_> {
    match message {
        UiMessage::AssistantMessage(message) => TimelineEntry::Blocks(
            message
                .blocks
                .iter()
                .map(|block| match block {
                    AssistantMessageBlock::Thinking { duration_ms, .. } => {
                        BlockView::Thinking(*duration_ms)
                    }
                    AssistantMessageBlock::Text { text } => BlockView::Text(text),
                    AssistantMessageBlock::ToolUse { id, name, input } => {
                        BlockView::ToolUse(Cow::Owned(ToolUseBlock {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        }))
                    }
                })
                .collect(),
            false,
        ),
        UiMessage::SystemEvent(UiSystemEvent::ToolResults { results }) => TimelineEntry::Blocks(
            results
                .iter()
                .map(|result| BlockView::Result(Cow::Owned(adapt_result(result))))
                .collect(),
            false,
        ),
        _ => TimelineEntry::Boundary(message),
    }
}

/// 流式块保持原顺序，供缓存尾部和全量校验使用。
pub fn project_pending(
    pending: &crate::features::timeline::model::StreamingMessage,
) -> TimelineEntry<'_> {
    TimelineEntry::Blocks(
        pending
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Thinking(thinking) => Some(BlockView::Thinking(thinking.duration_ms)),
                ContentBlock::Text(text) => Some(BlockView::Text(&text.text)),
                ContentBlock::ToolUse(tool) => Some(BlockView::ToolUse(Cow::Borrowed(tool))),
                ContentBlock::ToolResult(result) => Some(BlockView::Result(Cow::Borrowed(result))),
                ContentBlock::Image(_) => None,
            })
            .collect(),
        true,
    )
}

fn adapt_result(result: &ToolResultRecord) -> ToolResultBlock {
    ToolResultBlock {
        tool_use_id: result.tool_use_id.clone(),
        is_error: result.is_error,
        content: result.content.clone(),
        metadata: result.metadata.clone(),
    }
}
