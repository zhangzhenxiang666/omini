use crate::config::ThinkingEffort;
use crate::display::{DisplayPlan, HistoryItem};
use crate::message::{Message, ToolResultBlock, ToolUseBlock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveProfile {
    #[default]
    Main,
    Auto,
    Plan,
}

impl ActiveProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Auto => "auto",
            Self::Plan => "plan",
        }
    }
}

impl std::fmt::Display for ActiveProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub kind: NotificationKind,
    pub message: String,
    pub details: Vec<String>,
}

impl Notification {
    pub fn new(kind: NotificationKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: Vec::new(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(NotificationKind::Info, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(NotificationKind::Warn, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(NotificationKind::Error, message)
    }

    pub fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentStartedEvent {
    pub session_id: String,
    pub parent_session_id: String,
    pub spawn_tool_use_id: String,
    pub agent_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSnapshot {
    pub session_id: String,
    pub parent_session_id: String,
    pub spawn_tool_use_id: String,
    pub agent_label: String,
    pub status: SubagentStatus,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentMessageEvent {
    pub session_id: String,
    pub message: Message,
    #[serde(default)]
    pub persist_llm_history: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentToolUseEvent {
    pub session_id: String,
    pub tool_use: ToolUseBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentToolResultEvent {
    pub session_id: String,
    pub tool_result: ToolResultBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentFinishedEvent {
    pub session_id: String,
    pub status: SubagentStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    Auto,
    Manual,
}

impl CompactTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

impl std::fmt::Display for CompactTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactEvent {
    pub trigger: CompactTrigger,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryDeltaEvent {
    pub trigger: CompactTrigger,
    pub delta: String,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactShrinkFinishedEvent {
    pub trigger: CompactTrigger,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub before_messages: usize,
    pub after_messages: usize,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactShrinkFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFinishedEvent {
    pub trigger: CompactTrigger,
    pub summary: String,
    pub after_tokens: usize,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

pub type SubmittedPlan = DisplayPlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutionProfile {
    Main,
    Auto,
}

impl PlanExecutionProfile {
    pub fn active_profile(self) -> ActiveProfile {
        match self {
            Self::Main => ActiveProfile::Main,
            Self::Auto => ActiveProfile::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanApprovalAction {
    Approve { profile: PlanExecutionProfile },
    ApproveAndCompact { profile: PlanExecutionProfile },
    ContinueDiscussing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsageSnapshot {
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

impl From<SessionUsageSnapshot> for SessionUsage {
    fn from(usage: SessionUsageSnapshot) -> Self {
        Self {
            current_context_tokens: usage.current_context_tokens,
            total_tokens: usage.total_tokens,
            total_cached_tokens: usage.total_cached_tokens,
            context_window: usage.context_window,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedSession {
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<ThinkingEffort>,
    pub title: Option<String>,
    pub messages: Vec<HistoryItem>,
    pub subagents: Vec<SubagentSnapshot>,
    pub usage: SessionUsageSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolPauseRequest {
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_tool_use_id: Option<String>,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_source: Option<PermissionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
    pub kind: ToolPauseKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSource {
    pub decision: String,
    pub source: String,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolPauseKind {
    Permission(PermissionPreview),
    UserInput(UserInputPreview),
}

impl Serialize for ToolPauseKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct PermissionPayload<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            preview: &'a PermissionPreview,
        }

        #[derive(Serialize)]
        struct UserInputPayload<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            questions: &'a [UserInputQuestion],
        }

        match self {
            Self::Permission(preview) => PermissionPayload {
                kind: "permission",
                preview,
            }
            .serialize(serializer),
            Self::UserInput(preview) => UserInputPayload {
                kind: "user_input",
                questions: &preview.questions,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ToolPauseKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("missing tool pause kind type"))?;

        match kind {
            "permission" => {
                let preview = value.get("preview").ok_or_else(|| {
                    serde::de::Error::custom("permission tool pause missing preview")
                })?;
                serde_json::from_value(preview.clone())
                    .map(Self::Permission)
                    .map_err(serde::de::Error::custom)
            }
            "user_input" => serde_json::from_value(value)
                .map(Self::UserInput)
                .map_err(serde::de::Error::custom),
            "bash" | "edit" | "write" | "read" | "search" | "mcp" | "custom" => {
                serde_json::from_value(value)
                    .map(Self::Permission)
                    .map_err(serde::de::Error::custom)
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown tool pause kind type '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPauseResponse {
    Permission {
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    UserInput {
        value: Value,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionPreview {
    Bash(BashPermissionPreview),
    Edit(EditPermissionPreview),
    Write(EditPermissionPreview),
    Read(ReadPermissionPreview),
    Search(SearchPermissionPreview),
    Mcp(McpPermissionPreview),
    Custom {
        tool_name: String,
        payload: serde_json::Map<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashPermissionPreview {
    pub command: String,
    pub description: Option<String>,
    pub workdir: Option<String>,
    pub timeout: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadPermissionPreview {
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPermissionPreview {
    pub query: String,
    pub mode: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpPermissionPreview {
    pub server_name: String,
    pub server_tool_name: String,
    pub registered_tool_name: String,
    pub inputs: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPermissionPreview {
    pub summary: String,
    pub path: String,
    pub replacement_count: usize,
    pub replace_all: bool,
    pub start_lines: Vec<usize>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputPreview {
    pub questions: Vec<UserInputQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn active_profile_serializes_auto_value() {
        assert_eq!(ActiveProfile::Auto.as_str(), "auto");
        assert_eq!(
            serde_json::to_value(ActiveProfile::Auto).unwrap(),
            json!("auto")
        );
        assert_eq!(
            serde_json::from_value::<ActiveProfile>(json!("auto")).unwrap(),
            ActiveProfile::Auto
        );
    }

    #[test]
    fn plan_execution_profile_maps_to_active_profile() {
        assert_eq!(
            PlanExecutionProfile::Main.active_profile(),
            ActiveProfile::Main
        );
        assert_eq!(
            PlanExecutionProfile::Auto.active_profile(),
            ActiveProfile::Auto
        );
    }

    #[test]
    fn permission_response_deserializes_without_note() {
        let response: ToolPauseResponse = serde_json::from_value(json!({
            "type": "permission",
            "approved": false,
        }))
        .unwrap();

        assert_eq!(
            response,
            ToolPauseResponse::Permission {
                approved: false,
                note: None,
            }
        );
    }

    #[test]
    fn tool_pause_permission_wraps_preview_type() {
        let kind = ToolPauseKind::Permission(PermissionPreview::Write(edit_preview()));

        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            json!({
                "type": "permission",
                "preview": {
                    "type": "write",
                    "summary": "Create file",
                    "path": "/tmp/new.txt",
                    "replacement_count": 0,
                    "replace_all": false,
                    "start_lines": [],
                    "added_lines": 2,
                    "removed_lines": 0
                }
            })
        );
    }

    #[test]
    fn tool_pause_permission_deserializes_legacy_preview_type() {
        let kind: ToolPauseKind = serde_json::from_value(json!({
            "type": "write",
            "summary": "Create file",
            "path": "/tmp/new.txt",
            "replacement_count": 0,
            "replace_all": false,
            "start_lines": [],
            "added_lines": 2,
            "removed_lines": 0
        }))
        .unwrap();

        assert_eq!(
            kind,
            ToolPauseKind::Permission(PermissionPreview::Write(edit_preview()))
        );
    }

    fn edit_preview() -> EditPermissionPreview {
        EditPermissionPreview {
            summary: "Create file".to_string(),
            path: "/tmp/new.txt".to_string(),
            replacement_count: 0,
            replace_all: false,
            start_lines: Vec::new(),
            added_lines: 2,
            removed_lines: 0,
        }
    }

    #[test]
    fn session_usage_omits_empty_context_window_for_protocol_overlay() {
        let usage = SessionUsage::from(SessionUsageSnapshot {
            current_context_tokens: 1,
            total_tokens: 2,
            total_cached_tokens: 3,
            context_window: None,
        });

        assert_eq!(
            serde_json::to_value(usage).unwrap(),
            json!({
                "current_context_tokens": 1,
                "total_tokens": 2,
                "total_cached_tokens": 3
            })
        );
    }
}
