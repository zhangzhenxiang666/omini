use jiff::Timestamp;
use omini_model::message::Role;
use toasty::Deferred;

use super::Thread;

/// Provider 模型上下文表:按版本不可变,替换上下文时整体换版本。
/// 复合主键 (thread_id, context_version, ordinal) 的索引同时覆盖
/// "按线程+版本取出全部消息按 ordinal 排序"这一唯一查询模式。
#[derive(Debug, Clone, toasty::Model)]
#[table = "llm_messages"]
#[key(thread_id, context_version, ordinal)]
pub struct LlmMessage {
    pub thread_id: String,
    pub context_version: i64,
    /// 同版本内的稳定顺序。
    pub ordinal: i64,
    pub role: Role,
    pub content: String,
    pub created_at: Timestamp,
    #[belongs_to(key = thread_id, references = id)]
    pub thread: Deferred<Thread>,
}
