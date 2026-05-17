use crate::types::message::{ContentBlock, Message, Role};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionKind {
    Subagent,
    File,
    Directory,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryItem {
    Message(Message),
    Display(DisplayMessage),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSubmission {
    pub llm_message: Message,
    pub display_message: Option<DisplayMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDraft {
    pub text: String,
    pub mentions: Vec<DisplayMention>,
}

impl UserDraft {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            mentions: Vec::new(),
        }
    }

    pub fn into_submission(self) -> UserSubmission {
        let llm_text = build_llm_text(&self.text, &self.mentions);
        let llm_message = Message::new(
            Role::User,
            vec![ContentBlock::from_text(
                llm_text.unwrap_or_else(|| self.text.clone()),
            )],
        );
        let display_message = (!self.mentions.is_empty()).then_some(DisplayMessage {
            role: Role::User,
            text: self.text,
            mentions: self.mentions,
        });

        UserSubmission {
            llm_message,
            display_message,
        }
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
    let context_mentions = mentions
        .iter()
        .filter(|mention| mention.kind != MentionKind::Command)
        .collect::<Vec<_>>();
    if context_mentions.is_empty() {
        return None;
    }

    let mut output = text.to_string();
    output.push_str("\n\nReferenced context:\n");
    for mention in context_mentions {
        output.push_str("- ");
        output.push_str(&mention_llm_text(mention));
        output.push('\n');
    }
    Some(output)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_with_mentions_builds_split_submission() {
        let draft = UserDraft {
            text: "check @src/main.rs".to_string(),
            mentions: vec![DisplayMention {
                start_char: 6,
                end_char: 19,
                kind: MentionKind::File,
                label: "src/main.rs".to_string(),
                target: "src/main.rs".to_string(),
                description: "file".to_string(),
            }],
        };

        let submission = draft.into_submission();
        let display = submission.display_message.unwrap();
        assert_eq!(display.text, "check @src/main.rs");
        assert_eq!(display.mentions[0].target, "src/main.rs");

        let text = submission
            .llm_message
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(text.contains("Referenced context:"));
        assert!(text.contains("File: src/main.rs. Read this file if needed."));
    }

    #[test]
    fn plain_draft_builds_plain_submission() {
        let submission = UserDraft::plain("hello".to_string()).into_submission();
        assert!(submission.display_message.is_none());
        assert_eq!(
            submission.llm_message,
            Message::from_user_text("hello".to_string())
        );
    }

    #[test]
    fn command_mention_is_display_only() {
        let submission = UserDraft {
            text: "/init extra notes".to_string(),
            mentions: vec![DisplayMention {
                start_char: 0,
                end_char: 5,
                kind: MentionKind::Command,
                label: "init".to_string(),
                target: "init".to_string(),
                description: String::new(),
            }],
        }
        .into_submission();

        assert_eq!(
            submission.llm_message,
            Message::from_user_text("/init extra notes".to_string())
        );
        let display = submission.display_message.unwrap();
        assert_eq!(display.mentions[0].kind, MentionKind::Command);
    }
}
