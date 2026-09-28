use jiff::Timestamp;
use omini_domain::agent_run::AgentStepStatus;
use toasty::Deferred;

use super::AgentRun;

/// Agent Step 表:Run 内一次模型调用,`(run_id, step_no)` 唯一——
/// 该唯一索引同时是按 run_id 取 step 列表的前缀索引。
#[derive(Debug, Clone, toasty::Model)]
#[table = "agent_step"]
#[unique(run_id, step_no)]
pub struct AgentStep {
    #[key]
    pub id: String,
    pub run_id: String,
    pub step_no: i64,
    pub status: AgentStepStatus,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[belongs_to(key = run_id, references = id)]
    pub agent_run: Deferred<AgentRun>,
}
