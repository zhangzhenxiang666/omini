use crate::app::event::Notification;
use omini_domain::conversation::{
    AssistantMessage, AssistantMessageBlock, SystemEvent, TaskNotification, ToolResultRecord,
    UserInput,
};
use omini_domain::input::{InputPart, RunCommand, UserInputIntent};
use omini_model::message::Message;
use omini_model::message::{ContentBlock, Role};
use omini_protocol::HistoryItem;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayMention {
    pub start_char: usize,
    pub end_char: usize,
    pub kind: MentionKind,
    pub label: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayImageAttachment {
    pub start_char: usize,
    pub end_char: usize,
    pub marker: String,
    pub source_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionKind {
    Subagent,
    File,
    Directory,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UserDraft {
    pub text: String,
    pub mentions: Vec<DisplayMention>,
    pub images: Vec<DisplayImageAttachment>,
}

impl UserDraft {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            mentions: Vec::new(),
            images: Vec::new(),
        }
    }
}

pub fn user_input_draft(input: &UserInput) -> UserDraft {
    let mut text = String::new();
    let mut mentions = Vec::new();
    if let UserInputIntent::Command {
        command: RunCommand::Init,
    } = input.intent
    {
        text.push_str("/init");
    }
    for part in &input.parts {
        match part {
            InputPart::Text { text: value } => text.push_str(value),
            InputPart::Skill { name } => text.push_str(&format!("/{name}")),
            InputPart::File { path, label } => push_mention(
                &mut text,
                &mut mentions,
                MentionKind::File,
                label.as_deref().unwrap_or(path),
                path,
            ),
            InputPart::Directory { path, label } => push_mention(
                &mut text,
                &mut mentions,
                MentionKind::Directory,
                label.as_deref().unwrap_or(path),
                path,
            ),
            InputPart::Subagent { name, label } => push_mention(
                &mut text,
                &mut mentions,
                MentionKind::Subagent,
                label.as_deref().unwrap_or(name),
                name,
            ),
        }
    }
    for attachment in &input.attachments {
        if !text.is_empty() && !text.ends_with(char::is_whitespace) {
            text.push(' ');
        }
        text.push_str("[Image: ");
        text.push_str(&attachment.name);
        text.push(']');
    }
    UserDraft {
        text,
        mentions,
        images: Vec::new(),
    }
}

fn push_mention(
    text: &mut String,
    mentions: &mut Vec<DisplayMention>,
    kind: MentionKind,
    label: &str,
    target: &str,
) {
    let start_char = text.chars().count();
    text.push('@');
    text.push_str(label);
    mentions.push(DisplayMention {
        start_char,
        end_char: text.chars().count(),
        kind,
        label: label.to_string(),
        target: target.to_string(),
        description: String::new(),
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiMessage {
    UserInput(UserInput),
    AssistantMessage(AssistantMessage),
    SystemEvent(UiSystemEvent),
}

/// 持久化系统事件的 TUI 投影，以及只用于当前界面的运行时事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiSystemEvent {
    Plan { text: String },
    Summary { text: String },
    TaskNotification(TaskNotification),
    ToolResults { results: Vec<ToolResultRecord> },
    UserInputEcho(UserDraft),
    Notification(Notification),
    RunDivider { elapsed: Duration },
}

impl UiMessage {
    pub fn from_history_items(items: Vec<HistoryItem>) -> Vec<Self> {
        items.into_iter().map(Self::from_history_item).collect()
    }

    pub fn from_history_item(item: HistoryItem) -> Self {
        match item {
            HistoryItem::UserInput(input) => Self::UserInput(input),
            HistoryItem::AssistantMessage(message) => Self::AssistantMessage(message),
            HistoryItem::SystemEvent(event) => Self::SystemEvent(match event {
                SystemEvent::Plan(plan) => UiSystemEvent::Plan {
                    text: plan.markdown,
                },
                SystemEvent::Summary(summary) => UiSystemEvent::Summary {
                    text: summary.markdown,
                },
                SystemEvent::TaskNotification(notification) => {
                    UiSystemEvent::TaskNotification(notification)
                }
                SystemEvent::ToolResults { results } => UiSystemEvent::ToolResults { results },
            }),
        }
    }

    /// 在协议边界转换旧格式的消息；渲染只读取领域历史或流式块。
    pub fn from_model_message(message: Message) -> Vec<Self> {
        let content = match message.role {
            Role::Assistant => message.content,
            Role::User => message
                .content
                .into_iter()
                .filter(|block| matches!(block, ContentBlock::ToolResult(_)))
                .collect(),
        };
        Self::from_blocks(content)
    }

    pub fn from_blocks(content: Vec<ContentBlock>) -> Vec<Self> {
        let mut tool_results = Vec::new();
        let blocks = content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Thinking(block) => Some(AssistantMessageBlock::Thinking {
                    thinking: block.thinking,
                    duration_ms: block.duration_ms,
                }),
                ContentBlock::Text(block) => Some(AssistantMessageBlock::Text { text: block.text }),
                ContentBlock::ToolUse(block) => Some(AssistantMessageBlock::ToolUse {
                    id: block.id,
                    name: block.name,
                    input: block.input,
                }),
                ContentBlock::ToolResult(result) => {
                    tool_results.push(ToolResultRecord {
                        tool_use_id: result.tool_use_id,
                        is_error: result.is_error,
                        content: result.content,
                        metadata: result.metadata,
                    });
                    None
                }
                ContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>();
        let mut items = Vec::new();
        if !blocks.is_empty() {
            items.push(Self::AssistantMessage(AssistantMessage { blocks }));
        }
        if !tool_results.is_empty() {
            items.push(Self::SystemEvent(UiSystemEvent::ToolResults {
                results: tool_results,
            }));
        }
        items
    }
}

/// 流式尾部只持有服务端块，不带 Provider 的角色和消息封装。
#[derive(Debug, Clone, Default)]
pub struct StreamingMessage {
    pub content: Vec<ContentBlock>,
}
impl StreamingMessage {
    pub fn finish(self) -> Vec<UiMessage> {
        UiMessage::from_blocks(self.content)
    }
}
impl From<Message> for StreamingMessage {
    fn from(message: Message) -> Self {
        Self {
            content: message.content,
        }
    }
}
