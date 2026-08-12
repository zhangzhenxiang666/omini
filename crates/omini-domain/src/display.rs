use crate::message::{ContentBlock, Message, Role};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayMessage {
    pub role: Role,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<DisplayMention>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayPlan {
    pub id: String,
    pub title: String,
    pub markdown: String,
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplaySummary {
    pub id: String,
    pub title: String,
    pub markdown: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentTaskNotificationItem {
    pub task_id: String,
    pub agent: String,
    pub title: String,
    pub status: crate::events::AgentTaskStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentTaskNotification {
    pub tasks: Vec<AgentTaskNotificationItem>,
    pub created_at: DateTime<Utc>,
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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HistoryItem {
    Message(Message),
    Display(DisplayMessage),
    Plan(DisplayPlan),
    Summary(DisplaySummary),
    AgentTaskNotification(AgentTaskNotification),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UserSubmission {
    pub llm_message: Message,
    pub display_message: Option<DisplayMessage>,
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

    pub fn into_submission(self) -> Result<UserSubmission, String> {
        let llm_text = build_llm_text(&self.text, &self.mentions);
        let content = build_llm_content(llm_text.as_deref().unwrap_or(&self.text), &self.images)?;
        let llm_message = Message::new(Role::User, content);
        let display_message = (!self.mentions.is_empty()).then_some(DisplayMessage {
            role: Role::User,
            text: self.text,
            mentions: self.mentions,
        });

        Ok(UserSubmission {
            llm_message,
            display_message,
        })
    }

    pub fn history_item(self) -> HistoryItem {
        if self.mentions.is_empty() {
            HistoryItem::Message(Message::from_user_text(self.text))
        } else {
            HistoryItem::Display(DisplayMessage {
                role: Role::User,
                text: self.text,
                mentions: self.mentions,
            })
        }
    }
}

impl UserSubmission {
    pub fn plain(message: Message) -> Self {
        Self {
            llm_message: message,
            display_message: None,
        }
    }

    pub fn history_item(self) -> HistoryItem {
        match self.display_message {
            Some(display) => HistoryItem::Display(display),
            None => HistoryItem::Message(self.llm_message),
        }
    }
}

fn build_llm_text(text: &str, mentions: &[DisplayMention]) -> Option<String> {
    let context = referenced_context_text(mentions)?;
    Some(format!("{text}\n\n{context}"))
}

pub fn referenced_context_text(mentions: &[DisplayMention]) -> Option<String> {
    let context_mentions = mentions
        .iter()
        .filter(|mention| mention.kind != MentionKind::Command)
        .collect::<Vec<_>>();
    if context_mentions.is_empty() {
        return None;
    }

    let mut output = String::from("Referenced context:\n");
    for mention in context_mentions {
        output.push_str("- ");
        output.push_str(&mention_llm_text(mention));
        output.push('\n');
    }
    Some(output)
}

fn build_llm_content(
    text: &str,
    images: &[DisplayImageAttachment],
) -> Result<Vec<ContentBlock>, String> {
    let mut images = images.iter().collect::<Vec<_>>();
    images.sort_by_key(|image| image.start_char);

    let mut blocks = vec![ContentBlock::from_text(text.to_string())];
    for image in images {
        blocks.push(load_image_block(image)?);
    }

    Ok(blocks)
}

fn load_image_block(image: &DisplayImageAttachment) -> Result<ContentBlock, String> {
    let path = Path::new(&image.source_path);
    if !path.is_file() {
        return Err(format!("Image file does not exist: {}", image.source_path));
    }
    let media_type = image_media_type(path)
        .ok_or_else(|| format!("Unsupported image type: {}", image.source_path))?;
    let bytes = std::fs::read(path)
        .map_err(|err| format!("Failed to read image {}: {err}", image.source_path))?;
    Ok(ContentBlock::from_base64_image(
        media_type.to_string(),
        BASE64_STANDARD.encode(bytes),
    ))
}

fn image_media_type(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("webp") => Some("image/webp"),
        Some("gif") => Some("image/gif"),
        _ => None,
    }
}

fn mention_llm_text(mention: &DisplayMention) -> String {
    match mention.kind {
        MentionKind::Subagent => {
            if mention.description.trim().is_empty() {
                format!(
                    "Agent: @{}. Use subagent \"{}\" if this helps answer the user.",
                    mention.label, mention.target
                )
            } else {
                format!(
                    "Agent: @{} ({}). Use subagent \"{}\" if this helps answer the user.",
                    mention.label, mention.description, mention.target
                )
            }
        }
        MentionKind::Directory => format!(
            "Directory: {}. Inspect this directory if needed.",
            mention.target
        ),
        MentionKind::File => format!("File: {}. Read this file if needed.", mention.target),
        MentionKind::Command => unreachable!("command mentions do not add LLM context"),
    }
}
