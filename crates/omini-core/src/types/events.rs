use omini_domain::events::{
    CompactEvent, CompactShrinkFailedEvent, CompactShrinkFinishedEvent, CompactSummaryDeltaEvent,
    CompactSummaryFailedEvent, CompactSummaryFinishedEvent, SubagentFinishedEvent,
    SubagentMessageEvent, SubagentStartedEvent, SubagentToolResultEvent, SubagentToolUseEvent,
    ToolPauseRequest,
};
use omini_domain::message::{Message, ToolResultBlock, ToolUseBlock};
use omini_domain::usage::Usage;
use omini_runtime_contract::persistence::SessionRecord;

/// engine 发往 runtime 的 core 内部事件。
///
/// Runtime 消费这些事件后更新本地状态、增量持久化，并把外部可见更新转换为
/// `omini_runtime_contract::RuntimeToServerEvent`。
#[derive(Debug, Clone)]
pub enum EngineToRuntimeEvent {
    /// 一条 User Message 已进入引擎消息历史，需要按当前位置持久化。
    UserMessageProduced {
        message: Message,
        client_echo_id: Option<String>,
    },

    /// 引擎完成一轮流式输出，产出一条完整的 Assistant Message。
    MessageProduced(Message),

    /// 引擎收集完所有工具结果，打包成一条 User Message。
    ToolResultsProduced(Message),

    /// 一条只写入 LLM JSONL 历史、不会进入 SQLite/UI 历史的 Message。
    LlmHistoryProduced(Message),

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

    /// 子 agent 创建并开始运行。
    SubagentStarted(SubagentStartedEvent),
    /// 子 agent 会话元数据已创建，需要外部持久化。
    SubagentSessionCreated(SessionRecord),
    /// 子 agent 的一轮 LLM usage。
    SubagentUsageRecorded { session_id: String, usage: Usage },
    /// 子 agent 产生了一条完整消息，需要持久化并更新 UI 视图模型。
    SubagentMessageProduced(SubagentMessageEvent),
    /// 子 agent 请求工具调用。
    SubagentToolUse(SubagentToolUseEvent),
    /// 子 agent 工具执行完成。
    SubagentToolResult(SubagentToolResultEvent),
    /// 子 agent 运行结束。
    SubagentFinished(SubagentFinishedEvent),

    /// 引擎出错
    Error(String),
    /// 引擎运行时警告
    Warning(String),
}
