//! 工具暂停与计划审批协议。

use super::*;
use serde_json::Value;
use std::collections::HashMap;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutionProfile {
    Main,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanApprovalAction {
    Approve { profile: PlanExecutionProfile },
    ApproveInNewThread { profile: PlanExecutionProfile },
    ContinueDiscussing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PermissionSource {
    pub decision: String,
    pub source: String,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BashPermissionPreview {
    pub command: String,
    pub description: Option<String>,
    pub workdir: Option<String>,
    pub timeout: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EditPermissionPreview {
    pub summary: String,
    pub path: String,
    pub replacement_count: usize,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReadPermissionPreview {
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SearchPermissionPreview {
    pub query: String,
    pub mode: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct McpPermissionPreview {
    pub server_name: String,
    pub server_tool_name: String,
    pub registered_tool_name: String,
    pub inputs: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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
        payload: HashMap<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPauseKind {
    Permission { preview: PermissionPreview },
    UserInput { questions: Vec<UserInputQuestion> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ToolPauseRequest {
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_tool_use_id: Option<String>,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_source: Option<PermissionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
    pub kind: ToolPauseKind,
}

impl From<PlanExecutionProfile> for omini_runtime_contract::thread_domain::PlanExecutionProfile {
    fn from(profile: PlanExecutionProfile) -> Self {
        match profile {
            PlanExecutionProfile::Main => Self::Main,
            PlanExecutionProfile::Auto => Self::Auto,
        }
    }
}

impl From<omini_runtime_contract::thread_domain::PlanExecutionProfile> for PlanExecutionProfile {
    fn from(profile: omini_runtime_contract::thread_domain::PlanExecutionProfile) -> Self {
        match profile {
            omini_runtime_contract::thread_domain::PlanExecutionProfile::Main => Self::Main,
            omini_runtime_contract::thread_domain::PlanExecutionProfile::Auto => Self::Auto,
        }
    }
}

impl From<PlanApprovalAction> for omini_runtime_contract::thread_domain::PlanApprovalAction {
    fn from(action: PlanApprovalAction) -> Self {
        match action {
            PlanApprovalAction::Approve { profile } => Self::Approve {
                profile: profile.into(),
            },
            PlanApprovalAction::ApproveInNewThread { profile } => Self::ApproveInNewThread {
                profile: profile.into(),
            },
            PlanApprovalAction::ContinueDiscussing => Self::ContinueDiscussing,
        }
    }
}

impl From<omini_runtime_contract::thread_domain::PlanApprovalAction> for PlanApprovalAction {
    fn from(action: omini_runtime_contract::thread_domain::PlanApprovalAction) -> Self {
        match action {
            omini_runtime_contract::thread_domain::PlanApprovalAction::Approve { profile } => {
                Self::Approve {
                    profile: profile.into(),
                }
            }
            omini_runtime_contract::thread_domain::PlanApprovalAction::ApproveInNewThread {
                profile,
            } => Self::ApproveInNewThread {
                profile: profile.into(),
            },
            omini_runtime_contract::thread_domain::PlanApprovalAction::ContinueDiscussing => {
                Self::ContinueDiscussing
            }
        }
    }
}

impl From<ToolPauseResponse> for omini_runtime_contract::thread_domain::ToolPauseResponse {
    fn from(response: ToolPauseResponse) -> Self {
        match response {
            ToolPauseResponse::Permission { approved, note } => Self::Permission { approved, note },
            ToolPauseResponse::UserInput { value } => Self::UserInput { value },
            ToolPauseResponse::Cancelled => Self::Cancelled,
        }
    }
}

mod pause_conversion {
    use crate::pause as wire;
    use omini_runtime_contract::thread_domain as runtime;

    // 两侧字段名和含义相同时只列出转换边界，避免在运行时协议中复制传输依赖。
    macro_rules! bridge_fields {
        ($name:ident { $($field:ident),+ $(,)? }) => {
            impl From<runtime::$name> for wire::$name {
                fn from(value: runtime::$name) -> Self {
                    Self { $($field: value.$field),+ }
                }
            }
            impl From<wire::$name> for runtime::$name {
                fn from(value: wire::$name) -> Self {
                    Self { $($field: value.$field),+ }
                }
            }
        };
    }

    bridge_fields!(PermissionSource {
        decision,
        source,
        rule
    });
    bridge_fields!(BashPermissionPreview {
        command,
        description,
        workdir,
        timeout
    });
    bridge_fields!(EditPermissionPreview {
        summary,
        path,
        replacement_count,
        diff
    });
    bridge_fields!(ReadPermissionPreview { file_path });
    bridge_fields!(SearchPermissionPreview { query, mode, path });
    bridge_fields!(UserInputOption { label, description });

    impl From<runtime::McpPermissionPreview> for wire::McpPermissionPreview {
        fn from(value: runtime::McpPermissionPreview) -> Self {
            Self {
                server_name: value.server_name,
                server_tool_name: value.server_tool_name,
                registered_tool_name: value.registered_tool_name,
                inputs: value.inputs.into_iter().collect(),
            }
        }
    }

    impl From<wire::McpPermissionPreview> for runtime::McpPermissionPreview {
        fn from(value: wire::McpPermissionPreview) -> Self {
            Self {
                server_name: value.server_name,
                server_tool_name: value.server_tool_name,
                registered_tool_name: value.registered_tool_name,
                inputs: value.inputs.into_iter().collect(),
            }
        }
    }

    impl From<runtime::PermissionPreview> for wire::PermissionPreview {
        fn from(value: runtime::PermissionPreview) -> Self {
            match value {
                runtime::PermissionPreview::Bash(item) => Self::Bash(item.into()),
                runtime::PermissionPreview::Edit(item) => Self::Edit(item.into()),
                runtime::PermissionPreview::Write(item) => Self::Write(item.into()),
                runtime::PermissionPreview::Read(item) => Self::Read(item.into()),
                runtime::PermissionPreview::Search(item) => Self::Search(item.into()),
                runtime::PermissionPreview::Mcp(item) => Self::Mcp(item.into()),
                runtime::PermissionPreview::Custom { tool_name, payload } => Self::Custom {
                    tool_name,
                    payload: payload.into_iter().collect(),
                },
            }
        }
    }

    impl From<wire::PermissionPreview> for runtime::PermissionPreview {
        fn from(value: wire::PermissionPreview) -> Self {
            match value {
                wire::PermissionPreview::Bash(item) => Self::Bash(item.into()),
                wire::PermissionPreview::Edit(item) => Self::Edit(item.into()),
                wire::PermissionPreview::Write(item) => Self::Write(item.into()),
                wire::PermissionPreview::Read(item) => Self::Read(item.into()),
                wire::PermissionPreview::Search(item) => Self::Search(item.into()),
                wire::PermissionPreview::Mcp(item) => Self::Mcp(item.into()),
                wire::PermissionPreview::Custom { tool_name, payload } => Self::Custom {
                    tool_name,
                    payload: payload.into_iter().collect(),
                },
            }
        }
    }

    impl From<runtime::UserInputQuestion> for wire::UserInputQuestion {
        fn from(value: runtime::UserInputQuestion) -> Self {
            Self {
                id: value.id,
                header: value.header,
                question: value.question,
                options: value.options.into_iter().map(Into::into).collect(),
            }
        }
    }

    impl From<wire::UserInputQuestion> for runtime::UserInputQuestion {
        fn from(value: wire::UserInputQuestion) -> Self {
            Self {
                id: value.id,
                header: value.header,
                question: value.question,
                options: value.options.into_iter().map(Into::into).collect(),
            }
        }
    }

    impl From<runtime::ToolPauseKind> for wire::ToolPauseKind {
        fn from(value: runtime::ToolPauseKind) -> Self {
            match value {
                runtime::ToolPauseKind::Permission(preview) => Self::Permission {
                    preview: preview.into(),
                },
                runtime::ToolPauseKind::UserInput(preview) => Self::UserInput {
                    questions: preview.questions.into_iter().map(Into::into).collect(),
                },
            }
        }
    }

    impl From<wire::ToolPauseKind> for runtime::ToolPauseKind {
        fn from(value: wire::ToolPauseKind) -> Self {
            match value {
                wire::ToolPauseKind::Permission { preview } => Self::Permission(preview.into()),
                wire::ToolPauseKind::UserInput { questions } => {
                    Self::UserInput(runtime::UserInputPreview {
                        questions: questions.into_iter().map(Into::into).collect(),
                    })
                }
            }
        }
    }

    impl From<runtime::ToolPauseRequest> for wire::ToolPauseRequest {
        fn from(value: runtime::ToolPauseRequest) -> Self {
            Self {
                tool_use_id: value.tool_use_id,
                preview_tool_use_id: value.preview_tool_use_id,
                tool_name: value.tool_name,
                permission_source: value.permission_source.map(Into::into),
                source_thread_id: value.source_thread_id,
                source_agent_label: value.source_agent_label,
                kind: value.kind.into(),
            }
        }
    }

    impl From<wire::ToolPauseRequest> for runtime::ToolPauseRequest {
        fn from(value: wire::ToolPauseRequest) -> Self {
            Self {
                tool_use_id: value.tool_use_id,
                preview_tool_use_id: value.preview_tool_use_id,
                tool_name: value.tool_name,
                permission_source: value.permission_source.map(Into::into),
                source_thread_id: value.source_thread_id,
                source_agent_label: value.source_agent_label,
                kind: value.kind.into(),
            }
        }
    }
}
