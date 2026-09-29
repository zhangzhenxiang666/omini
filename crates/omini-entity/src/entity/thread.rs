use crate::StoreError;
use jiff::Timestamp;
use omini_domain::config::ThinkingEffort;
use omini_runtime_contract::persistence::ThreadRecord;
use std::str::FromStr;
use toasty::Deferred;

use super::{Message, Project};

/// 线程类型:主线或子 Agent 线程。
#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
pub enum ThreadType {
    Main,
    Agent,
}

impl ThreadType {
    /// 运行时记录的字符串形态到枚举;未知值按数据损坏拒绝。
    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "main" => Ok(Self::Main),
            "agent" => Ok(Self::Agent),
            other => Err(StoreError::InvalidData(format!(
                "unknown thread type '{other}'"
            ))),
        }
    }
}

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
    /// 创建本子线程的 spawn 工具调用;主线为 `None`。
    pub spawn_tool_use_id: Option<String>,
    pub thread_type: ThreadType,
    /// 子 Agent 的名称标签;主线为 `None`。
    pub agent_label: Option<String>,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<ThinkingEffort>,
    /// 显示标题;`None` 表示尚未生成初始标题。
    pub title: Option<String>,
    /// 最近一次模型调用的上下文 token 占用(覆盖写,非累计)。
    pub current_context_tokens: i64,
    /// 线程累计消耗的 token。
    pub total_tokens: i64,
    /// 累计命中缓存的 token。
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

/// 运行时线程记录到模型实例的映射;`thread_type` 字符串在此解析,
/// 未知形态按数据损坏拒绝而非静默默认。
pub fn thread_from_runtime(project_id: &str, thread: &ThreadRecord) -> Result<Thread, StoreError> {
    Ok(Thread {
        id: thread.id.clone(),
        project_id: project_id.to_string(),
        parent_thread_id: thread.parent_thread_id.clone(),
        spawn_tool_use_id: thread.spawn_tool_use_id.clone(),
        thread_type: ThreadType::parse(&thread.thread_type)?,
        agent_label: thread.agent_label.clone(),
        provider: thread.provider.clone(),
        model: thread.model.clone(),
        thinking_effort: parse_thinking_effort(thread.thinking_effort.as_deref())?,
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
    })
}

/// 思考力度字符串(运行时记录/事件契约携带)到枚举;非法值按数据损坏拒绝。
pub fn parse_thinking_effort(value: Option<&str>) -> Result<Option<ThinkingEffort>, StoreError> {
    value
        .map(|value| {
            ThinkingEffort::from_str(value)
                .map_err(|()| StoreError::InvalidData(format!("unknown thinking effort '{value}'")))
        })
        .transpose()
}
