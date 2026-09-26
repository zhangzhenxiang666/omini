use omini_domain::conversation::{
    AssistantMessage, AssistantMessageBlock, ToolResultRecord, UserInput,
};
use omini_domain::input::{InputPart, RunCommand, UserInputIntent};
use omini_model::message::{ContentBlock, Message, Role};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayMessage {
    pub role: Role,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<DisplayMention>,
}

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

    pub fn display_message(&self) -> DisplayMessage {
        DisplayMessage {
            role: Role::User,
            text: self.text.clone(),
            mentions: self.mentions.clone(),
        }
    }
}

pub fn user_input_message(input: &UserInput) -> DisplayMessage {
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
    DisplayMessage {
        role: Role::User,
        text,
        mentions,
    }
}

pub fn assistant_message(output: &AssistantMessage) -> Message {
    let content = output
        .blocks
        .iter()
        .map(|block| match block {
            AssistantMessageBlock::Thinking {
                thinking,
                duration_ms,
            } => ContentBlock::Thinking(omini_model::message::ThinkingBlock {
                thinking: thinking.clone(),
                duration_ms: *duration_ms,
            }),
            AssistantMessageBlock::Text { text } => ContentBlock::from_text(text.clone()),
            AssistantMessageBlock::ToolUse { id, name, input } => {
                ContentBlock::from_tool_use(id.clone(), name.clone(), input.clone())
            }
        })
        .collect();
    Message::new(Role::Assistant, content)
}

pub fn tool_results_message(results: &[ToolResultRecord]) -> Message {
    let content = results
        .iter()
        .map(|result| {
            ContentBlock::ToolResult(omini_model::message::ToolResultBlock {
                tool_use_id: result.tool_use_id.clone(),
                is_error: result.is_error,
                content: result.content.clone(),
                metadata: result.metadata.clone(),
            })
        })
        .collect();
    Message::new(Role::User, content)
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
