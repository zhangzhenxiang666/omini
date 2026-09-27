//! 运行请求和响应协议。

use super::*;
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Queued,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStepStatus {
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolUseStatus {
    Pending,
    WaitingApproval,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentRunSnapshot {
    pub id: String,
    pub thread_id: String,
    pub parent_run_id: Option<String>,
    pub status: AgentRunStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub total_tokens: i64,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentStepSnapshot {
    pub id: String,
    pub run_id: String,
    pub step_no: u32,
    pub status: AgentStepStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ToolUseExecutionSnapshot {
    pub id: String,
    pub step_id: String,
    pub name: String,
    pub input: Value,
    pub status: ToolUseStatus,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AgentRunsResponse {
    pub runs: Vec<AgentRunSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AgentRunDetailResponse {
    pub run: AgentRunSnapshot,

    pub steps: Vec<AgentStepSnapshot>,

    pub tool_uses: Vec<ToolUseExecutionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ArchiveAgentRunRequest {
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AgentRunInputRequest {
    pub input: UserInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_echo_id: Option<String>,
}

/// 向线程提交用户输入或运行中插入输入的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmitRunRequest {
    SubmitMessage {
        input: UserInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    InterveneMessage {
        input: UserInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    ExecuteCommand {
        command: RunCommand,
        input: UserInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
}

/// 运行请求已被接受后的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunSubmittedResponse {
    pub run_id: String,
}

/// 客户端响应工具暂停请求的请求体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResolveToolPauseRequest {
    pub response: ToolPauseResponse,
}

/// 客户端审批或拒绝模型提交计划的请求体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResolvePlanRequest {
    pub action: PlanApprovalAction,
}

impl From<omini_domain::agent_run::AgentRunStatus> for AgentRunStatus {
    fn from(status: omini_domain::agent_run::AgentRunStatus) -> Self {
        match status {
            omini_domain::agent_run::AgentRunStatus::Queued => Self::Queued,
            omini_domain::agent_run::AgentRunStatus::Running => Self::Running,
            omini_domain::agent_run::AgentRunStatus::WaitingApproval => Self::WaitingApproval,
            omini_domain::agent_run::AgentRunStatus::Completed => Self::Completed,
            omini_domain::agent_run::AgentRunStatus::Failed => Self::Failed,
            omini_domain::agent_run::AgentRunStatus::Cancelled => Self::Cancelled,
            omini_domain::agent_run::AgentRunStatus::Interrupted => Self::Interrupted,
        }
    }
}

impl From<AgentRunStatus> for omini_domain::agent_run::AgentRunStatus {
    fn from(status: AgentRunStatus) -> Self {
        match status {
            AgentRunStatus::Queued => Self::Queued,
            AgentRunStatus::Running => Self::Running,
            AgentRunStatus::WaitingApproval => Self::WaitingApproval,
            AgentRunStatus::Completed => Self::Completed,
            AgentRunStatus::Failed => Self::Failed,
            AgentRunStatus::Cancelled => Self::Cancelled,
            AgentRunStatus::Interrupted => Self::Interrupted,
        }
    }
}

impl From<AgentRunSnapshot> for omini_domain::agent_run::AgentRunSnapshot {
    fn from(run: AgentRunSnapshot) -> Self {
        Self {
            id: run.id,
            thread_id: run.thread_id,
            parent_run_id: run.parent_run_id,
            status: run.status.into(),
            created_at: run.created_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
            total_tokens: run.total_tokens,
            archived_at: run.archived_at,
        }
    }
}

impl From<omini_domain::agent_run::AgentStepStatus> for AgentStepStatus {
    fn from(status: omini_domain::agent_run::AgentStepStatus) -> Self {
        match status {
            omini_domain::agent_run::AgentStepStatus::Running => Self::Running,
            omini_domain::agent_run::AgentStepStatus::WaitingApproval => Self::WaitingApproval,
            omini_domain::agent_run::AgentStepStatus::Completed => Self::Completed,
            omini_domain::agent_run::AgentStepStatus::Failed => Self::Failed,
            omini_domain::agent_run::AgentStepStatus::Cancelled => Self::Cancelled,
            omini_domain::agent_run::AgentStepStatus::Interrupted => Self::Interrupted,
        }
    }
}

impl From<omini_domain::agent_run::ToolUseStatus> for ToolUseStatus {
    fn from(status: omini_domain::agent_run::ToolUseStatus) -> Self {
        match status {
            omini_domain::agent_run::ToolUseStatus::Pending => Self::Pending,
            omini_domain::agent_run::ToolUseStatus::WaitingApproval => Self::WaitingApproval,
            omini_domain::agent_run::ToolUseStatus::Running => Self::Running,
            omini_domain::agent_run::ToolUseStatus::Completed => Self::Completed,
            omini_domain::agent_run::ToolUseStatus::Failed => Self::Failed,
            omini_domain::agent_run::ToolUseStatus::Cancelled => Self::Cancelled,
        }
    }
}

impl From<omini_domain::agent_run::AgentRunSnapshot> for AgentRunSnapshot {
    fn from(run: omini_domain::agent_run::AgentRunSnapshot) -> Self {
        Self {
            id: run.id,
            thread_id: run.thread_id,
            parent_run_id: run.parent_run_id,
            status: run.status.into(),
            created_at: run.created_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
            total_tokens: run.total_tokens,
            archived_at: run.archived_at,
        }
    }
}

impl From<omini_domain::agent_run::AgentStepSnapshot> for AgentStepSnapshot {
    fn from(step: omini_domain::agent_run::AgentStepSnapshot) -> Self {
        Self {
            id: step.id,
            run_id: step.run_id,
            step_no: step.step_no,
            status: step.status.into(),
            started_at: step.started_at,
            finished_at: step.finished_at,
            input_tokens: step.input_tokens,
            output_tokens: step.output_tokens,
        }
    }
}

impl From<omini_domain::agent_run::ToolUseExecutionSnapshot> for ToolUseExecutionSnapshot {
    fn from(tool: omini_domain::agent_run::ToolUseExecutionSnapshot) -> Self {
        Self {
            id: tool.id,
            step_id: tool.step_id,
            name: tool.name,
            input: tool.input,
            status: tool.status.into(),
            updated_at: tool.updated_at,
        }
    }
}
