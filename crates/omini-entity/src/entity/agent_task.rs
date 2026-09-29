use jiff::Timestamp;
use omini_domain::task::TaskStatus;
use omini_runtime_contract::thread_domain::{AgentTaskExecutionMode, AgentTaskResult};
use toasty::stmt::Json;

/// 子 Agent 任务表:任务身份与结果,`agent_thread_id` 唯一。
/// 其余 5 个 FK 只保留标量列:查询均按列过滤,声明关系无导航收益。
/// `(owner_thread_id, created_at)` 支撑主线程按时间列出任务;
/// `result` 经 ORM 的 Json 列自动编解码(存储为 JSON TEXT)。
#[derive(Debug, Clone, toasty::Model)]
#[table = "agent_task"]
#[index(name = "idx_agent_task_owner", owner_thread_id, created_at)]
pub struct AgentTask {
    #[key]
    pub task_id: String,
    pub owner_thread_id: String,
    #[unique]
    pub agent_thread_id: String,
    pub parent_run_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub parent_thread_id: String,
    pub spawn_tool_use_id: String,
    /// 派生深度。
    pub depth: i64,
    pub execution_mode: AgentTaskExecutionMode,
    pub status: TaskStatus,
    pub agent_name: String,
    pub title: String,
    #[column(type = text)]
    pub result: Option<Json<AgentTaskResult>>,
    #[auto]
    pub created_at: Timestamp,
    #[auto]
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub notification_delivered: bool,
}
