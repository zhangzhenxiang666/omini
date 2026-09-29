use jiff::Timestamp;
use omini_domain::task::TaskStatus;
use omini_runtime_contract::thread_domain::{AgentTaskExecutionMode, AgentTaskResult};
use toasty::stmt::Json;

/// 子 Agent 任务表:任务身份与结果,`agent_thread_id` 唯一。
/// 其余 5 个 FK 只保留标量列:查询均按列过滤,声明关系无导航收益。
/// `(owner_thread_id, created_at)` 支撑主线程按时间列出任务;
#[derive(Debug, Clone, toasty::Model)]
#[table = "agent_task"]
#[index(name = "idx_agent_task_owner", owner_thread_id, created_at)]
pub struct AgentTask {
    #[key]
    pub task_id: String,
    /// 任务归属的主线程。
    pub owner_thread_id: String,
    #[unique]
    pub agent_thread_id: String,
    /// 发起本任务的父 Run。
    pub parent_run_id: Option<String>,
    /// 发起者所在任务;主线程直接发起为 `None`。
    pub parent_task_id: Option<String>,
    /// 发起本任务的线程(一级任务为主线程,二级为一级任务的子线程)。
    pub parent_thread_id: String,
    /// 创建本任务的 spawn 工具调用。
    pub spawn_tool_use_id: String,
    /// 派生深度:主线程直接派生为 1,上限 `MAX_AGENT_DEPTH`(当前 2)。
    pub depth: i64,
    pub execution_mode: AgentTaskExecutionMode,
    pub status: TaskStatus,
    pub agent_name: String,
    pub title: String,
    /// 终态写入的任务结果;未终态为 `None`。
    #[column(type = text)]
    pub result: Option<Json<AgentTaskResult>>,
    #[auto]
    pub created_at: Timestamp,
    #[auto]
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    /// 完成通知是否已写入时间线。
    pub notification_delivered: bool,
}
