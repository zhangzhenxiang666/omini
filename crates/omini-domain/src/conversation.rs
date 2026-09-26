use crate::input::{AttachmentMetadata, InputPart, UserInputIntent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;

/// 用户提交的原始语义输入，不包含模型展开后的消息。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct UserInput {
    pub intent: UserInputIntent,
    pub parts: Vec<InputPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentMetadata>,
}

/// 面向会话时间线的助手输出块，不携带模型消息的角色或 Provider 传输结构。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageBlock {
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: std::collections::HashMap<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AssistantMessage {
    pub blocks: Vec<AssistantMessageBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProposedPlan {
    pub id: String,
    pub title: String,
    pub markdown: String,
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CompactionSummary {
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
    pub status: crate::task::TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentTaskNotification {
    pub tasks: Vec<AgentTaskNotificationItem>,
    pub created_at: DateTime<Utc>,
}

/// 系统执行工具后产生的结果；在模型上下文中对应 User 角色消息。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ToolResultRecord {
    pub tool_use_id: String,
    pub is_error: bool,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemEvent {
    Plan(ProposedPlan),
    Summary(CompactionSummary),
    AgentTaskNotification(AgentTaskNotification),
    ToolResults { results: Vec<ToolResultRecord> },
}

/// 一条持久化会话时间线记录；模型上下文消息由独立类型表示。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", content = "content", rename_all = "snake_case")]
pub enum ConversationEntry {
    UserInput(UserInput),
    AssistantMessage(AssistantMessage),
    SystemEvent(SystemEvent),
}
