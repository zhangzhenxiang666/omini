use jiff::Timestamp;

/// 投递状态机:登记 → 注入成功或判定不可投递。
#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
pub enum DeliveryStatus {
    Pending,
    Injected,
    Failed,
}

/// 任务消息投递表:登记待注入子 Agent 模型上下文的消息并做幂等去重。
/// 复合主键 (task_id, source_kind, source_key):相同来源键重试只接受一次,
/// 该键同时生成复合 upsert 访问器(or_ignore 即 INSERT OR IGNORE 语义)。
/// `source_kind` 保持 String:与运行时契约的 DeliveryKey 键形态对齐;
/// `payload` 是裸 `serde_json::Value` 列,幂等比对按结构相等。
#[derive(Debug, Clone, toasty::Model)]
#[table = "agent_task_delivery"]
#[key(task_id, source_kind, source_key)]
#[index(name = "idx_delivery_owner", owner_thread_id)]
pub struct AgentTaskDelivery {
    pub task_id: String,
    /// `agent`(主 Agent send_message)或 `client`(TUI 输入)。
    pub source_kind: String,
    pub source_key: String,
    pub owner_thread_id: String,
    pub agent_thread_id: String,
    #[column(type = text)]
    pub payload: serde_json::Value,
    pub status: DeliveryStatus,
    pub failure_reason: Option<String>,
    #[auto]
    pub created_at: Timestamp,
    #[auto]
    pub updated_at: Timestamp,
}
