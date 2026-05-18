use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TextBlock {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ImageBlock {
    pub source: ImageSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: ImageSourceType,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSourceType {
    Base64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub is_error: bool,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ThinkingBlock {
    pub thinking: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Thinking(ThinkingBlock),
    Text(TextBlock),
    Image(ImageBlock),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::User => write!(f, "user"),
            Role::Assistant => write!(f, "assistant"),
        }
    }
}

impl ContentBlock {
    pub fn from_thinking(thinking: String) -> Self {
        Self::Thinking(ThinkingBlock { thinking })
    }

    pub fn from_text(text: String) -> Self {
        Self::Text(TextBlock { text })
    }

    pub fn from_base64_image(media_type: String, data: String) -> Self {
        Self::Image(ImageBlock {
            source: ImageSource {
                source_type: ImageSourceType::Base64,
                media_type,
                data,
            },
        })
    }

    pub fn from_tool_use(id: String, name: String, input: HashMap<String, Value>) -> Self {
        Self::ToolUse(ToolUseBlock { id, name, input })
    }

    pub fn from_tool_result(tool_use_id: String, is_error: bool, output: String) -> Self {
        Self::ToolResult(ToolResultBlock {
            tool_use_id,
            is_error,
            content: output,
            metadata: None,
        })
    }

    pub fn is_text(&self) -> bool {
        matches!(self, ContentBlock::Text(_))
    }

    pub fn is_image(&self) -> bool {
        matches!(self, ContentBlock::Image(_))
    }

    pub fn is_tool_use(&self) -> bool {
        matches!(self, ContentBlock::ToolUse(_))
    }

    pub fn is_tool_result(&self) -> bool {
        matches!(self, ContentBlock::ToolResult(_))
    }

    pub fn is_thinking(&self) -> bool {
        matches!(self, ContentBlock::Thinking(_))
    }
}

impl Message {
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }

    pub fn from_user_text(text: String) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text(TextBlock { text })],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_block_serializes_as_anthropic_shape() {
        let block = ContentBlock::from_base64_image("image/png".to_string(), "abc123".to_string());

        assert_eq!(
            serde_json::to_value(&block).unwrap(),
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "abc123",
                },
            })
        );
        assert!(block.is_image());
    }

    #[test]
    fn image_block_deserializes_from_anthropic_shape() {
        let block: ContentBlock = serde_json::from_value(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/jpeg",
                "data": "def456",
            },
        }))
        .unwrap();

        assert_eq!(
            block,
            ContentBlock::Image(ImageBlock {
                source: ImageSource {
                    source_type: ImageSourceType::Base64,
                    media_type: "image/jpeg".to_string(),
                    data: "def456".to_string(),
                },
            })
        );
    }
}
