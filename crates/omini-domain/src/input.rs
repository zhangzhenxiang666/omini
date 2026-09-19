use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputPart {
    Text {
        text: String,
    },
    Skill {
        name: String,
    },
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Directory {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Subagent {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunCommand {
    Init,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserInputIntent {
    Message,
    Command { command: RunCommand },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AttachmentMetadata {
    pub attachment_id: String,
    pub mime_type: String,
    pub size: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ResolvedAttachment {
    #[serde(flatten)]
    pub metadata: AttachmentMetadata,
    pub sha256: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RuntimeUserInput {
    pub parts: Vec<InputPart>,
    pub attachments: Vec<ResolvedAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayUserInput {
    pub role: crate::message::Role,
    pub intent: UserInputIntent,
    pub parts: Vec<InputPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentMetadata>,
}

impl DisplayUserInput {
    /// 用户级展示视图是原始 typed input 的纯投影，由输入接收方（server）构建；
    /// LLM 上下文行由 core 在安全边界提交，两条数据流各自独立。
    pub fn from_runtime_input(input: &RuntimeUserInput, intent: UserInputIntent) -> Self {
        let mut attachments = input
            .attachments
            .iter()
            .map(|attachment| attachment.metadata.clone())
            .collect::<Vec<_>>();
        attachments.sort_by(|left, right| left.attachment_id.cmp(&right.attachment_id));
        Self {
            role: crate::message::Role::User,
            intent,
            parts: input.parts.clone(),
            attachments,
        }
    }

    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                InputPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn display_message(&self) -> crate::display::DisplayMessage {
        use crate::display::MentionKind;

        let mut text = String::new();
        let mut mentions = Vec::new();
        if let UserInputIntent::Command {
            command: RunCommand::Init,
        } = self.intent
        {
            text.push_str("/init");
        }
        for part in &self.parts {
            match part {
                InputPart::Text { text: value } => text.push_str(value),
                InputPart::Skill { name } => text.push_str(&format!("/{name}")),
                InputPart::File { path, label } => push_display_mention(
                    &mut text,
                    &mut mentions,
                    MentionKind::File,
                    label.as_deref().unwrap_or(path),
                    path,
                ),
                InputPart::Directory { path, label } => push_display_mention(
                    &mut text,
                    &mut mentions,
                    MentionKind::Directory,
                    label.as_deref().unwrap_or(path),
                    path,
                ),
                InputPart::Subagent { name, label } => push_display_mention(
                    &mut text,
                    &mut mentions,
                    MentionKind::Subagent,
                    label.as_deref().unwrap_or(name),
                    name,
                ),
            }
        }
        for attachment in &self.attachments {
            if !text.is_empty() && !text.ends_with(char::is_whitespace) {
                text.push(' ');
            }
            text.push_str("[Image: ");
            text.push_str(&attachment.name);
            text.push(']');
        }
        crate::display::DisplayMessage {
            role: self.role.clone(),
            text,
            mentions,
        }
    }
}

fn push_display_mention(
    text: &mut String,
    mentions: &mut Vec<crate::display::DisplayMention>,
    kind: crate::display::MentionKind,
    label: &str,
    target: &str,
) {
    let start_char = text.chars().count();
    text.push('@');
    text.push_str(label);
    let end_char = text.chars().count();
    mentions.push(crate::display::DisplayMention {
        start_char,
        end_char,
        kind,
        label: label.to_string(),
        target: target.to_string(),
        description: String::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_projection_keeps_parts_raw_and_sorts_attachments() {
        let attachment = |id: &str| ResolvedAttachment {
            metadata: AttachmentMetadata {
                attachment_id: id.to_string(),
                mime_type: "image/png".to_string(),
                size: 1,
                name: format!("{id}.png"),
            },
            sha256: "0".repeat(64),
            source_path: PathBuf::from(id),
        };
        let input = RuntimeUserInput {
            parts: vec![
                InputPart::Skill {
                    name: "ordered".to_string(),
                },
                InputPart::Text {
                    text: "hello".to_string(),
                },
            ],
            attachments: vec![attachment("b"), attachment("a")],
        };

        let display = DisplayUserInput::from_runtime_input(&input, UserInputIntent::Message);

        assert_eq!(display.parts, input.parts);
        assert_eq!(
            display
                .attachments
                .iter()
                .map(|metadata| metadata.attachment_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        // display 只保留原始 parts 与附件元数据，不包含 skill 展开后的正文。
        assert_eq!(
            display.display_message().text,
            "/orderedhello [Image: a.png] [Image: b.png]"
        );
    }

    #[test]
    fn init_command_display_shows_slash_invocation() {
        let input = RuntimeUserInput {
            parts: Vec::new(),
            attachments: Vec::new(),
        };

        let display = DisplayUserInput::from_runtime_input(
            &input,
            UserInputIntent::Command {
                command: RunCommand::Init,
            },
        );

        assert_eq!(display.display_message().text, "/init");
    }
}
