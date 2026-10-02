//! core 与宿主（server）之间的窄异步接口。
//!
//! core 通过这里的两个职责与外界交互：
//! - 执行持久化：core 表达"发生了什么领域事实"，宿主负责落库并返回明确结果；
//! - 执行资源申请：core 表达子 Agent 任务与父子关系，宿主原子建立子会话并
//!   提供存储资源（thread 目录句柄）。
//!
//! 接口不暴露 SQLite 实体、数据库行形状或客户端协议；调用结果同步返回，
//! core 在关键持久化成功后才推进依赖它的状态。

use crate::CoreError;
use jiff::Timestamp;
use omini_config::project::ThreadDir;
use omini_domain::agent_run::{
    AgentRunSnapshot, AgentRunStatus, AgentStepSnapshot, AgentStepStatus, ToolUseExecutionSnapshot,
    ToolUseStatus,
};
use omini_domain::conversation::{
    AgentMessage, CompactionSummary, ProposedPlan, TaskNotification, UserInput,
};
use omini_domain::task::{TaskInfo, TaskStatus};
use omini_domain::usage::Usage;
use omini_model::message::Message;
use omini_runtime_contract::thread_domain::{AgentTaskExecutionMode, AgentTaskResult, DeliveryKey};

/// 宿主操作失败原因；core 将其转为错误事件或任务警告，不区分底层存储类型。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{context}: {message}")]
pub struct HostError {
    context: &'static str,
    message: String,
}

impl HostError {
    pub fn new(context: &'static str, message: impl Into<String>) -> Self {
        Self {
            context,
            message: message.into(),
        }
    }

    pub fn context(&self) -> &'static str {
        self.context
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<HostError> for CoreError {
    fn from(error: HostError) -> Self {
        CoreError::persistence(error.context(), error.message().to_string())
    }
}

/// 子 Agent 会话创建申请：core 表达任务、父子关系与所需资源，
/// thread 身份与目录由宿主分配和建立。
#[derive(Debug, Clone)]
pub struct AgentSessionRequest {
    /// core 分配的任务身份，同时用作子 Run ID 与任务键。
    pub task_id: String,
    pub parent_run_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub owner_thread_id: String,
    pub parent_thread_id: String,
    pub spawn_tool_use_id: String,
    pub agent: String,
    pub title: String,
    pub depth: u8,
    pub execution_mode: AgentTaskExecutionMode,
    /// 任务初始展示输入（进入子会话 UI 历史）。
    pub initial_prompt: UserInput,
    /// 初始 LLM 上下文消息（进入子会话模型历史）。
    pub initial_message: Message,
    /// 子会话使用的有效模型，宿主据此组装子线程行。
    pub model: AgentSessionModel,
}

/// 子会话线程行的模型字段；行形状由宿主派生，core 只表达有效配置。
#[derive(Debug, Clone)]
pub struct AgentSessionModel {
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<String>,
}

/// 宿主成功建立的子 Agent 会话资源。
#[derive(Debug, Clone)]
pub struct AgentSession {
    /// 宿主分配的子线程稳定 ID。
    pub thread_id: String,
    /// 已建立的子线程目录句柄。
    pub thread_dir: ThreadDir,
}

/// Agent 执行宿主接口。
///
/// 所有方法都是幂等语义明确的操作：core 发起请求并等待结果，成功即已落库，
/// 失败即本次操作未生效（宿主保证原子性），core 不重试。
#[async_trait::async_trait]
pub trait AgentHost: Send + Sync {
    // ===== 主线程执行持久化 =====

    /// 记录一次 Agent Run 的创建（内部续跑等非预留运行）。
    async fn create_agent_run(&self, run: &AgentRunSnapshot) -> Result<(), HostError>;

    /// 更新 Run 状态、起止时间与累计 Token。
    async fn update_agent_run(
        &self,
        run_id: &str,
        status: AgentRunStatus,
        started_at: Option<Timestamp>,
        finished_at: Option<Timestamp>,
        add_tokens: i64,
    ) -> Result<(), HostError>;

    async fn upsert_agent_step(&self, step: &AgentStepSnapshot) -> Result<(), HostError>;

    async fn update_agent_step(
        &self,
        step_id: &str,
        status: AgentStepStatus,
        finished_at: Option<Timestamp>,
        add_input_tokens: i64,
        add_output_tokens: i64,
    ) -> Result<(), HostError>;

    async fn upsert_tool_use_execution(
        &self,
        tool_use: &ToolUseExecutionSnapshot,
        status: ToolUseStatus,
    ) -> Result<(), HostError>;

    /// 追加一条模型上下文消息（不进 UI 历史）。
    async fn append_llm_message(&self, thread_id: &str, message: &Message)
    -> Result<(), HostError>;

    /// 追加一条 UI 展示消息（不进模型上下文）。
    async fn append_ui_message(
        &self,
        thread_id: &str,
        message: &Message,
        model_ref: Option<&str>,
    ) -> Result<(), HostError>;

    async fn insert_plan_message(
        &self,
        thread_id: &str,
        plan: &ProposedPlan,
        model_ref: &str,
    ) -> Result<(), HostError>;

    async fn insert_compact_summary(
        &self,
        thread_id: &str,
        summary: &CompactionSummary,
        model_ref: &str,
    ) -> Result<(), HostError>;

    /// 原子替换模型上下文；返回替换后的新上下文版本号。
    async fn replace_llm_context(
        &self,
        thread_id: &str,
        expected_version: i64,
        messages: Vec<Message>,
    ) -> Result<i64, HostError>;

    async fn record_thread_usage(&self, thread_id: &str, usage: Usage) -> Result<(), HostError>;

    /// 累计线程总用量（不覆盖当前上下文用量）。
    async fn record_thread_total_usage(
        &self,
        thread_id: &str,
        usage: Usage,
    ) -> Result<(), HostError>;

    /// 把子 Agent 用量归属累计到 owner 线程总量。
    async fn record_owner_agent_usage(
        &self,
        owner_thread_id: &str,
        usage: Usage,
    ) -> Result<(), HostError>;

    async fn update_thread_config(
        &self,
        thread_id: &str,
        provider: &str,
        model: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), HostError>;

    async fn update_thread_thinking_effort(
        &self,
        thread_id: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), HostError>;

    /// 刷新线程 updated_at（运行开始等边界）。
    async fn touch_thread(&self, thread_id: &str) -> Result<(), HostError>;

    /// 创建或更新通用后台任务索引。
    async fn upsert_background_task(&self, task: &TaskInfo) -> Result<(), HostError>;

    /// 持久化任务完成通知；返回本次实际写入的新通知内容。
    ///
    /// 幂等语义：通知中的已交付任务会被过滤，仅未交付
    /// 部分写入 UI 与模型上下文历史并出现在返回值中；没有新任务时返回
    /// `None` 且不落库、不广播，调用方不得据此再次投影 UI。
    async fn insert_task_notification(
        &self,
        owner_thread_id: &str,
        notification: &TaskNotification,
    ) -> Result<Option<TaskNotification>, HostError>;

    // ===== 子 Agent 会话与消息 =====

    /// 原子建立子会话（子线程、任务、Run 与初始历史）并返回存储资源。
    async fn create_agent_session(
        &self,
        request: AgentSessionRequest,
    ) -> Result<AgentSession, HostError>;

    /// 持久化子会话消息；成功后调用方才对外发布对应提交事件。
    async fn persist_agent_message(
        &self,
        agent_thread_id: &str,
        message: &Message,
        model_ref: Option<&str>,
        persist_llm_history: bool,
        display_in_ui: bool,
    ) -> Result<(), HostError>;

    /// 主 Agent 消息登记为待投递记录；来源键重复的幂等重试由宿主吸收并
    /// 自行完成展示投影，core 仅在落库成功后继续。
    async fn enqueue_agent_message(
        &self,
        task_id: &str,
        owner_thread_id: &str,
        agent_thread_id: &str,
        message: &AgentMessage,
    ) -> Result<(), HostError>;

    /// 安全边界按来源键注入模型历史并结算投递。
    async fn inject_task_message(
        &self,
        key: &DeliveryKey,
        agent_thread_id: &str,
        model_message: &Message,
    ) -> Result<(), HostError>;

    /// 任务终止时结算未注入消息；返回结算条数。
    async fn fail_pending_task_messages(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<u32, HostError>;

    /// 持久化任务终态；成功后调用方才发布终态事件与完成通知。
    async fn finish_agent_task(
        &self,
        task_id: &str,
        status: TaskStatus,
        result: &AgentTaskResult,
        completed_at: Timestamp,
    ) -> Result<(), HostError>;

    async fn set_agent_tasks_cancelling(&self, task_ids: &[String]) -> Result<(), HostError>;
}
