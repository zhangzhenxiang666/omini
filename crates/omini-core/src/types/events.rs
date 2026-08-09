use omini_domain::display::AgentTaskNotification;
use omini_domain::events::{
    CompactEvent, CompactShrinkFailedEvent, CompactShrinkFinishedEvent, CompactSummaryDeltaEvent,
    CompactSummaryFailedEvent, CompactSummaryFinishedEvent, ToolPauseRequest,
};
use omini_domain::message::{Message, ToolResultBlock, ToolUseBlock};
use omini_domain::usage::Usage;
use tokio::sync::oneshot;

/// engine 发往 runtime 的 core 内部事件。
///
/// Runtime 消费这些事件后更新本地状态、增量持久化，并把外部可见更新转换为
/// `omini_runtime_contract::RuntimeToServerEvent`。
#[derive(Debug)]
pub enum EngineToRuntimeEvent {
    /// 一条 User Message 已进入引擎消息历史，需要按当前位置持久化。
    UserMessageProduced {
        message: Message,
        client_echo_id: Option<String>,
    },

    /// Agent task completion 已到达安全输入边界，等待原子持久化后进入内存历史。
    AgentTaskNotificationsProduced {
        notification: AgentTaskNotification,
        llm_message: Message,
        task_ids: Vec<String>,
        ack: oneshot::Sender<Result<(), String>>,
    },

    /// 引擎完成一轮流式输出，产出一条完整的 Assistant Message。
    MessageProduced(Message),

    /// 引擎收集完所有工具结果，打包成一条 User Message。
    ToolResultsProduced(Message),

    /// 一条只写入 LLM 上下文、不会进入 UI 历史的 Message。
    LlmHistoryProduced(Message),

    /// compact 产生的新完整上下文，持久化成功后才允许调用方切换内存版本。
    ReplaceLlmContext {
        thread_id: String,
        expected_version: i64,
        messages: Vec<Message>,
        ack: oneshot::Sender<Result<i64, String>>,
    },

    /// 工具结果的 UI/SQLite 展示消息，不写入 LLM JSONL 历史。
    ToolResultsDisplayProduced(Message),

    /// 当前轮完整结束（助理消息 + 工具结果均已产出）。
    TurnEnded,

    /// 新一轮 LLM 调用开始
    TurnStarted,
    /// thinking 块流式增量
    ThinkingDelta(String),
    /// text 块流式增量
    TextDelta(String),
    /// LLM 请求工具调用
    ToolUse(ToolUseBlock),
    /// 工具执行结果
    ToolResult(ToolResultBlock),

    /// 工具需要暂停等待用户授权或输入
    ToolPauseRequested(Box<ToolPauseRequest>),
    /// 当前 engine/session 的一轮 LLM usage。
    UsageRecorded(Usage),
    /// 当前 engine/session 开始快速收缩上下文。
    CompactShrinkStarted(CompactEvent),
    /// 当前 engine/session 完成快速收缩上下文。
    CompactShrinkFinished(CompactShrinkFinishedEvent),
    /// 当前 engine/session 快速收缩上下文失败。
    CompactShrinkFailed(CompactShrinkFailedEvent),
    /// 当前 engine/session 开始 LLM 压缩摘要。
    CompactSummaryStarted(CompactEvent),
    /// 当前 engine/session 正在流式输出压缩摘要。
    CompactSummaryDelta(CompactSummaryDeltaEvent),
    /// 当前 engine/session 完成 LLM 压缩摘要。
    CompactSummaryFinished(CompactSummaryFinishedEvent),
    /// 当前 engine/session LLM 压缩摘要失败。
    CompactSummaryFailed(CompactSummaryFailedEvent),
    /// 当前 engine/session 的 LLM 摘要 usage。
    CompactSummaryUsageRecorded(Usage),
    /// 引擎出错
    Error(String),
    /// 引擎运行时警告
    Warning(String),
}
