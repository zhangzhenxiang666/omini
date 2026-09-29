use jiff::Timestamp;
use omini_domain::agent_run::ToolUseStatus;
use toasty::Deferred;

use super::AgentStep;

/// 工具调用执行表:同一 Step 下的每次 ToolUse 记录。
/// `input` 是裸 `serde_json::Value` 列,ORM 直接以 JSON TEXT 编解码。
#[derive(Debug, Clone, toasty::Model)]
#[table = "tool_use_execution"]
#[index(name = "idx_tool_use_step", step_id)]
pub struct ToolUseExecution {
    #[key]
    pub id: String,
    pub step_id: String,
    pub name: String,
    #[column(type = text)]
    pub input: serde_json::Value,
    pub status: ToolUseStatus,
    #[auto]
    pub updated_at: Timestamp,
    #[belongs_to(key = step_id, references = id)]
    pub step: Deferred<AgentStep>,
}
