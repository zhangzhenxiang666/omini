use jiff::Timestamp;
use omini_model::message::Role;
use toasty::Deferred;

use super::Thread;

/// 消息形态:普通块消息或序列化的会话条目。
#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
pub enum MessageKind {
    /// ContentBlock 数组的 JSON 序列化。
    Normal,
    /// ConversationEntry 的 JSON 序列化。
    ConversationEntry,
}

/// 用户可见消息表:UI 时间线的持久化形态。
#[derive(Debug, Clone, toasty::Model)]
#[table = "messages"]
pub struct Message {
    #[key]
    #[auto]
    pub id: i64,
    #[index]
    pub thread_id: String,
    pub role: Role,
    /// assistant 消息必填、其他角色必须为 None(写入方约定)。
    pub model_ref: Option<String>,
    pub content: String,
    pub kind: MessageKind,
    pub created_at: Timestamp,
    #[belongs_to(key = thread_id, references = id)]
    pub thread: Deferred<Thread>,
}
