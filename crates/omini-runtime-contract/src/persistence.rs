use crate::thread_domain::{AgentTaskInfo, AgentTaskResult};
use chrono::{DateTime, Utc};
use omini_domain::agent_run::{
    AgentRunSnapshot, AgentRunStatus, AgentStepSnapshot, AgentStepStatus, ToolUseExecutionSnapshot,
    ToolUseStatus,
};
use omini_domain::conversation::{AgentTaskNotification, CompactionSummary, ProposedPlan};
use omini_domain::task::{TaskInfo, TaskStatus};
use omini_domain::usage::Usage;
use omini_model::message::Message;
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct ThreadRecord {
    pub id: String,
    pub parent_thread_id: Option<String>,
    pub spawn_tool_use_id: Option<String>,
    pub thread_type: String,
    pub agent_label: Option<String>,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<String>,
    pub title: Option<String>,
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    pub llm_context_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// core 发往 server 的持久化意图词汇表。
///
/// 描述"发生了什么领域事实、要如何入库"，不是 SQL schema 的镜像：行级时间戳由
/// server 在应用事件时打点（持久化通道严格有序，打点位置不影响顺序语义），字段里
/// 保留的时间戳（`ThreadRecord`、任务 `completed_at` 等）本身是领域事实。
/// role/kind 等字符串形状的 SQL 词汇由 server 从领域类型派生。
#[derive(Debug)]
pub enum RuntimePersistenceEvent {
    CreateAgentRun {
        run: Box<AgentRunSnapshot>,
    },
    UpdateAgentRun {
        run_id: String,
        status: AgentRunStatus,
        started_at: Option<DateTime<Utc>>,
        finished_at: Option<DateTime<Utc>>,
        add_tokens: i64,
    },
    UpsertAgentStep {
        step: AgentStepSnapshot,
    },
    UpdateAgentStep {
        step_id: String,
        status: AgentStepStatus,
        finished_at: Option<DateTime<Utc>>,
        add_input_tokens: i64,
        add_output_tokens: i64,
    },
    UpsertToolUseExecution {
        tool_use: ToolUseExecutionSnapshot,
        status: ToolUseStatus,
    },
    SetAgentRunArchived {
        run_id: String,
        archived_at: Option<DateTime<Utc>>,
    },
    /// 原子创建子线程、task 记录和初始用户消息。
    CreateAgentTask {
        task: Box<AgentTaskInfo>,
        thread: ThreadRecord,
        initial_message: Message,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// 在发送 `message_committed` 流式事件前持久化子线程消息。
    PersistAgentMessage {
        thread_id: String,
        message: Message,
        model_ref: Option<String>,
        persist_llm_history: bool,
        display_in_ui: bool,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// 持久化通道严格有序，因此只有全部子线程消息处理完后才会提交终态。
    FinishAgentTask {
        task_id: String,
        status: TaskStatus,
        result: AgentTaskResult,
        completed_at: DateTime<Utc>,
        ack: oneshot::Sender<Result<(), String>>,
    },
    SetAgentTasksCancelling {
        task_ids: Vec<String>,
    },
    /// 创建或更新通用后台任务索引，不包含执行器专属数据。
    UpsertTask {
        task: TaskInfo,
    },
    InsertAgentTaskNotification {
        owner_thread_id: String,
        notification: AgentTaskNotification,
        llm_message: Message,
        task_ids: Vec<String>,
        ack: oneshot::Sender<Result<(), String>>,
    },
    UpdateThreadUpdatedAt {
        thread_id: String,
    },
    UpdateThreadConfig {
        thread_id: String,
        provider: String,
        model: String,
        thinking_effort: Option<String>,
    },
    UpdateThreadThinkingEffort {
        thread_id: String,
        thinking_effort: Option<String>,
    },
    /// 一条写入 UI 历史的展示消息。`message.content` 是 core 按 active_profile
    /// 剥离 plan 块后的 UI 展示块；role/kind/行时间戳等 SQL 形状由 server 派生。
    UiMessageAppended {
        thread_id: String,
        message: Message,
        model_ref: Option<String>,
    },
    InsertPlanMessage {
        thread_id: String,
        plan: ProposedPlan,
        model_ref: String,
    },
    InsertCompactSummaryMessage {
        thread_id: String,
        summary: CompactionSummary,
        model_ref: String,
    },
    AppendLlmMessage {
        thread_id: String,
        message: Message,
    },
    ReplaceLlmContext {
        thread_id: String,
        expected_version: i64,
        messages: Vec<Message>,
        ack: oneshot::Sender<Result<i64, String>>,
    },
    RecordThreadUsage {
        thread_id: String,
        usage: Usage,
    },
    RecordThreadTotalUsage {
        thread_id: String,
        usage: Usage,
    },
    RecordOwnerAgentUsage {
        thread_id: String,
        usage: Usage,
    },
}
