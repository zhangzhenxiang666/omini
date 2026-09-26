use crate::input::{AttachmentMetadata, InputPart, UserInputIntent};
use crate::task::TaskCompletion;
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
pub struct TaskNotification {
    pub tasks: Vec<TaskCompletion>,
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
    TaskNotification(TaskNotification),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskCompletion, TaskKind, TaskStatus};
    use chrono::TimeZone;

    #[test]
    fn task_notification_uses_generic_tag_and_rejects_the_old_tag() {
        let notification =
            ConversationEntry::SystemEvent(SystemEvent::TaskNotification(TaskNotification {
                tasks: vec![TaskCompletion {
                    task_id: "bash-1".to_string(),
                    kind: TaskKind::Bash,
                    label: "Bash".to_string(),
                    title: "Run command".to_string(),
                    status: TaskStatus::Completed,
                    summary: None,
                }],
                created_at: chrono::Utc.with_ymd_and_hms(2026, 9, 26, 0, 0, 0).unwrap(),
            }));
        let value = serde_json::to_value(&notification).unwrap();
        assert_eq!(value["content"]["type"], "task_notification");
        assert_eq!(value["content"]["tasks"][0]["kind"], "bash");

        let old_entry = serde_json::json!({
            "kind": "system_event",
            "content": {
                "type": "agent_task_notification",
                "tasks": [],
                "created_at": "2026-09-26T00:00:00Z"
            }
        });
        assert!(serde_json::from_value::<ConversationEntry>(old_entry).is_err());
    }
}
