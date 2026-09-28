use jiff::Timestamp;
use omini_domain::agent_run::AgentRunStatus;
use toasty::Deferred;

use super::{AgentStep, Thread};

/// Agent Run 表:一次 Agent 运行的治理记录,可经 `parent_run_id` 关联父 Run。
/// Run 只归档不删除;`(thread_id, created_at)` 支撑线程内按时间列出。
#[derive(Debug, Clone, toasty::Model)]
#[table = "agent_run"]
#[index(name = "idx_agent_run_thread", thread_id, created_at)]
pub struct AgentRun {
    #[key]
    pub id: String,
    pub thread_id: String,
    /// 自引用仅做标量列:查询只判断有无父 Run,不导航。
    pub parent_run_id: Option<String>,
    pub status: AgentRunStatus,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub total_tokens: i64,
    /// 归档标记;非 NULL 表示已从默认列表隐藏。
    pub archived_at: Option<Timestamp>,
    #[belongs_to(key = thread_id, references = id)]
    pub thread: Deferred<Thread>,
    #[has_many]
    pub steps: Deferred<Vec<AgentStep>>,
}
