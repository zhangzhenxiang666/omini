use jiff::Timestamp;
use omini_runtime_contract::persistence::ThreadRecord;
use toasty::Deferred;

use super::{Message, Project};

/// 线程表:含主线与子 Agent 线程,`parent_thread_id` 组成树。
/// 树删除由业务代码逐层收集后代后在事务内完成(数据库无级联)。
#[derive(Debug, Clone, toasty::Model)]
#[table = "thread"]
#[index(name = "idx_thread_project", project_id)]
#[index(name = "idx_thread_parent", parent_thread_id)]
pub struct Thread {
    #[key]
    pub id: String,
    pub project_id: String,
    /// 父线程;子线程随父树一起删除。
    pub parent_thread_id: Option<String>,
    pub spawn_tool_use_id: Option<String>,
    /// `main` 或 `agent`。
    pub thread_type: String,
    pub agent_label: Option<String>,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<String>,
    pub title: Option<String>,
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    /// LLM 上下文乐观并发版本,替换上下文的事务内校验后递增。
    pub llm_context_version: i64,
    #[auto]
    pub created_at: Timestamp,
    #[auto]
    pub updated_at: Timestamp,
    #[belongs_to(key = project_id, references = id)]
    pub project: Deferred<Project>,
    /// 自引用父线程;`pair` 消除与 children 的反向歧义。
    #[belongs_to(key = parent_thread_id, references = id)]
    pub parent: Deferred<Option<Thread>>,
    #[has_many(pair = parent)]
    pub children: Deferred<Vec<Thread>>,
    /// 线程的用户可见消息(导航用;列表查询仍按 FK 列过滤)。
    #[has_many]
    pub messages: Deferred<Vec<Message>>,
}

/// 运行时线程记录到模型实例的机械映射(子 Agent 任务创建路径使用)。
pub fn thread_from_runtime(project_id: &str, thread: &ThreadRecord) -> Thread {
    Thread {
        id: thread.id.clone(),
        project_id: project_id.to_string(),
        parent_thread_id: thread.parent_thread_id.clone(),
        spawn_tool_use_id: thread.spawn_tool_use_id.clone(),
        thread_type: thread.thread_type.clone(),
        agent_label: thread.agent_label.clone(),
        provider: thread.provider.clone(),
        model: thread.model.clone(),
        thinking_effort: thread.thinking_effort.clone(),
        title: thread.title.clone(),
        current_context_tokens: thread.current_context_tokens,
        total_tokens: thread.total_tokens,
        total_cached_tokens: thread.total_cached_tokens,
        llm_context_version: thread.llm_context_version,
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        // 关系字段在 create! 时省略,只写上面的 FK 标量列。
        project: Default::default(),
        parent: Default::default(),
        children: Default::default(),
        messages: Default::default(),
    }
}
