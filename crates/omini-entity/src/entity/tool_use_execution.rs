use jiff::Timestamp;
use omini_domain::agent_run::ToolUseStatus;
use toasty::Deferred;

use super::AgentStep;

/// 工具调用执行表:同一 Step 下的每次 ToolUse 记录。
/// `input` 以 JSON TEXT 存储,序列化由业务代码负责。
#[derive(Debug, Clone, toasty::Model)]
#[table = "tool_use_execution"]
#[index(name = "idx_tool_use_step", step_id)]
pub struct ToolUseExecution {
    #[key]
    pub id: String,
    pub step_id: String,
    pub name: String,
    pub input_json: String,
    pub status: ToolUseStatus,
    #[auto]
    pub updated_at: Timestamp,
    #[belongs_to(key = step_id, references = id)]
    pub step: Deferred<AgentStep>,
}
