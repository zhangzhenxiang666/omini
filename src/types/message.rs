use serde::{Deserialize, Serialize};
use serde_json::Value;
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
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub is_error: bool,
    pub output: String,
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
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl ContentBlock {
    pub fn from_thinking(thinking: String) -> Self {
        Self::Thinking(ThinkingBlock { thinking })
    }

    pub fn from_text(text: String) -> Self {
        Self::Text(TextBlock { text })
    }

    pub fn from_tool_use(id: String, name: String, input: HashMap<String, Value>) -> Self {
        Self::ToolUse(ToolUseBlock { id, name, input })
    }

    pub fn from_tool_result(tool_use_id: String, is_error: bool, output: String) -> Self {
        Self::ToolResult(ToolResultBlock {
            tool_use_id,
            is_error,
            output,
        })
    }

    pub fn is_text(&self) -> bool {
        matches!(self, ContentBlock::Text(_))
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
