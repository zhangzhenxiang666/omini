use crate::StoreError;
use jiff::Timestamp;

/// 投递来源:主 Agent send_message 或 TUI 客户端输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
pub enum SourceKind {
    Agent,
    Client,
}

impl SourceKind {
    /// 运行时契约 DeliveryKey 的字符串形态到枚举;未知值按数据损坏拒绝。
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "agent" => Ok(Self::Agent),
            "client" => Ok(Self::Client),
            other => Err(StoreError::InvalidData(format!(
                "unknown delivery source kind '{other}'"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Client => "client",
        }
    }
}

/// 投递状态机:登记 → 注入成功或判定不可投递。
#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
pub enum DeliveryStatus {
    Pending,
    Injected,
    Failed,
}

/// 任务消息投递表:登记待注入子 Agent 模型上下文的消息并做幂等去重。
/// 复合主键 (task_id, source_kind, source_key):相同来源键重试只接受一次,
/// 该键同时生成复合 upsert 访问器(or_ignore 即 INSERT OR IGNORE 语义);
/// `payload` 是裸 `serde_json::Value` 列,幂等比对按结构相等。运行时契约
/// 的 DeliveryKey 以字符串携带来源,经 SourceKind::parse/as_str 在边界转换。
#[derive(Debug, Clone, toasty::Model)]
#[table = "agent_task_delivery"]
#[key(task_id, source_kind, source_key)]
#[index(name = "idx_delivery_owner", owner_thread_id)]
pub struct AgentTaskDelivery {
    pub task_id: String,
    pub source_kind: SourceKind,
    pub source_key: String,
    /// 任务归属的主线程,待投递记录按它列出。
    pub owner_thread_id: String,
    /// 消息注入的目标子 Agent 线程。
    pub agent_thread_id: String,
    #[column(type = text)]
    pub payload: serde_json::Value,
    pub status: DeliveryStatus,
    /// 判定不可投递的原因;仅 `Failed` 状态有值。
    pub failure_reason: Option<String>,
    #[auto]
    pub created_at: Timestamp,
    #[auto]
    pub updated_at: Timestamp,
}
