use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 后台任务的通用执行类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    SubAgent,
    Bash,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SubAgent => "sub_agent",
            Self::Bash => "bash",
        }
    }
}

/// 后台任务在运行时和持久化层共享的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

/// 任务状态变化事件。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TaskChangedEvent {
    pub task: TaskInfo,
}

/// 增量命令输出所属的标准流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutputStream {
    Stdout,
    Stderr,
}

/// 通过 runtime 事件通道发送的后台命令输出增量。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TaskOutputDelta {
    pub task_id: String,
    pub tool_use_id: String,
    pub stream: TaskOutputStream,
    pub delta: String,
}

/// 所有后台任务共用的可查询记录；执行器专属数据仍由对应适配器保存。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TaskInfo {
    pub task_id: String,
    pub owner_thread_id: String,
    pub kind: TaskKind,
    pub title: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
}

/// 通用后台任务完成事实，由对应执行器提供面向 owner 的简短标签和摘要。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TaskCompletion {
    pub task_id: String,
    pub label: String,
    pub title: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}
