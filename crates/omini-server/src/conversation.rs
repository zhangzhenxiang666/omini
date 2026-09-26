use omini_domain::conversation::{
    AssistantMessage, AssistantMessageBlock, ConversationEntry, SystemEvent, ToolResultRecord,
};
use omini_model::message::{ContentBlock, Message, Role};
use omini_protocol::HistoryItem;

pub(crate) fn history_item_from_model_message(message: Message) -> HistoryItem {
    match entry_from_model_message(message.clone()) {
        Some(entry) => entry.into(),
        None => HistoryItem::UserInput(omini_domain::conversation::UserInput {
            intent: omini_domain::input::UserInputIntent::Message,
            parts: message
                .content
                .into_iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => {
                        Some(omini_domain::input::InputPart::Text { text: text.text })
                    }
                    _ => None,
                })
                .collect(),
            attachments: Vec::new(),
        }),
    }
}

pub(crate) fn entry_from_model_message(message: Message) -> Option<ConversationEntry> {
    if message.role == Role::Assistant {
        let assistant = assistant_from_model_message(message);
        return (!assistant.blocks.is_empty())
            .then_some(ConversationEntry::AssistantMessage(assistant));
    }

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
        .collect::<Vec<_>>();
    (!results.is_empty()).then_some(ConversationEntry::SystemEvent(SystemEvent::ToolResults {
        results,
    }))
}

fn assistant_from_model_message(message: Message) -> AssistantMessage {
    let blocks = message
        .content
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Thinking(block) => Some(AssistantMessageBlock::Thinking {
                thinking: block.thinking,
                duration_ms: block.duration_ms,
            }),
            ContentBlock::Text(block) => Some(AssistantMessageBlock::Text { text: block.text }),
            ContentBlock::Image(_) => None,
            ContentBlock::ToolUse(block) => Some(AssistantMessageBlock::ToolUse {
                id: block.id,
                name: block.name,
                input: block.input,
            }),
            ContentBlock::ToolResult(_) => None,
        })
        .collect();
    AssistantMessage { blocks }
}

pub(crate) fn domain_entry(item: HistoryItem) -> ConversationEntry {
    match item {
        HistoryItem::UserInput(input) => ConversationEntry::UserInput(input),
        HistoryItem::AssistantMessage(output) => ConversationEntry::AssistantMessage(output),
        HistoryItem::SystemEvent(output) => ConversationEntry::SystemEvent(output),
    }
}

pub(crate) fn model_message_from_assistant_message(
    output: AssistantMessage,
    role: omini_model::message::Role,
) -> Message {
    use omini_model::message::{ContentBlock, ThinkingBlock};
    let blocks = output
        .blocks
        .into_iter()
        .map(|block| match block {
            AssistantMessageBlock::Thinking {
                thinking,
                duration_ms,
            } => ContentBlock::Thinking(ThinkingBlock {
                thinking,
                duration_ms,
            }),
            AssistantMessageBlock::Text { text } => ContentBlock::from_text(text),
            AssistantMessageBlock::ToolUse { id, name, input } => {
                ContentBlock::from_tool_use(id, name, input)
            }
        })
        .collect();
    Message::new(role, blocks)
}
