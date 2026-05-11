use crate::types::config::ProviderProfile;
use crate::types::config::ThinkingEffort;
use crate::types::message::{Message, ToolResultBlock, ToolUseBlock};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;

// ===========================================================================
// 第一层：UI → Runtime 的请求
// ===========================================================================

/// UI → Runtime 的请求。
#[derive(Debug)]
pub enum UiRequest {
    /// 用户取消当前正在运行的对话
    CancelRun,
    /// 用户发送一条消息给 runtime
    SendMessage(String),
    /// 用户在模型选择页中确认选择
    ModelSelected {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
    },
    /// 用户在会话选择页中确认选择
    SessionSelected { session_id: String },
}

// ===========================================================================
// 第二层：Engine → Runtime 的内部事件
// ===========================================================================

/// Engine → Runtime 的内部事件。
///
/// Runtime 消费此事件后负责：
/// 1. 更新内部 `messages` 状态
/// 2. 增量持久化
/// 3. 翻译为 `RuntimeEvent` 转发给 UI
#[derive(Debug)]
pub enum EngineEvent {
    /// 引擎完成一轮流式输出，产出一条完整的 Assistant Message。
    MessageProduced(Message),

    /// 引擎收集完所有工具结果，打包成一条 User Message。
    ToolResultsProduced(Message),

    /// 当前轮完整结束（助理消息 + 工具结果均已产出）。
    /// Runtime 收到后转发 `RuntimeEvent::TurnEnded` 给 UI。
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

    /// 工具需要用户授权
    PermissionRequest(PermissionRequest),
    /// LLM 向用户提问
    UserConfirmation(UserConfirmation),

    /// 引擎出错
    Error(String),
}

// ===========================================================================
// 第三层：Runtime → UI 的事件
// ===========================================================================

/// Runtime → UI 的事件。
#[derive(Debug)]
pub enum RuntimeEvent {
    /// 用户输入已提交，运行时开始处理
    RunStarted,
    /// 所有轮次完成，运行结束
    RunFinished,

    /// 请求关闭整个程序
    Shutdown,

    /// 命令产生的文本输出（显示在消息区）
    CommandOutput(String),

    /// 模型已切换（TUI 更新状态栏用）
    ModelChanged {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
    },
    /// 会话已切换
    SessionChanged {
        session_id: Option<String>,
        messages: Vec<Message>,
    },

    /// 会话标题变更（TUI 头部栏显示用）
    SessionTitleChanged { title: Option<String> },
    /// 需要 TUI 弹出交互选择页
    InteractionRequest(InteractionRequest),

    /// Runtime 启动时推送命令列表（供自动补全使用）
    CommandList(Vec<CommandSummary>),

    /// 新一轮 LLM 调用开始
    TurnStarted,
    /// 当前轮 LLM 调用结束（所有 content block 已收齐）
    TurnEnded,

    /// thinking 块流式增量
    ThinkingDelta(String),
    /// text 块流式增量
    TextDelta(String),

    /// LLM 发起了工具调用
    ToolUse(ToolUseBlock),
    /// 工具执行完成，产出结果
    ToolResult(ToolResultBlock),

    /// 执行工具前需要用户授权
    PermissionRequest(PermissionRequest),
    /// LLM 向用户提问
    UserConfirmation(UserConfirmation),

    /// 运行时出错
    Error(String),
}

// ===========================================================================
// 命令系统相关类型
// ===========================================================================

/// 交互请求（Runtime → TUI，触发选择页）。
#[derive(Debug, Clone)]
pub enum InteractionRequest {
    /// 模型选择：列出所有提供商及模型
    ModelSelection {
        providers: HashMap<String, ProviderProfile>,
        current_provider: String,
        current_model: String,
    },
    /// 会话选择：列出项目下所有会话
    SessionSelection { sessions: Vec<SessionSummary> },
}

/// 会话摘要（供选择页展示）。
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub provider: String,
    pub message_count: i64,
    pub created_at: DateTime<Utc>,
}

/// 命令摘要（供自动补全 / 帮助展示）。
#[derive(Debug, Clone)]
pub struct CommandSummary {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    /// true = 需要额外参数，选中后只补全命令名+空格
    /// false = 无参数，选中后直接执行
    pub has_args: bool,
    pub args_description: Option<&'static str>,
}

/// 命令执行结果。
#[derive(Debug)]
pub enum CommandResult {
    Done,
    Pending,
    Error(String),
}

// ===========================================================================
// 共享类型
// ===========================================================================

/// 工具权限请求。
/// 当一个工具需要用户授权才能执行时发起此请求。
/// UI 展示授权对话框后，通过 `reply` 发送用户的决定。
#[derive(Debug)]
pub struct PermissionRequest {
    /// 工具名称
    pub tool_name: String,
    /// 工具输入参数
    pub tool_input: HashMap<String, Value>,
    /// 发送 `true` = 允许, `false` = 拒绝
    pub reply: tokio::sync::oneshot::Sender<bool>,
}

/// LLM 向用户提问。
/// 当 LLM 调用 ask_user 等工具需要用户输入时发起此请求。
/// UI 展示输入框后，通过 `reply` 发送用户的文字回复。
#[derive(Debug)]
pub struct UserConfirmation {
    /// LLM 提出的问题
    pub question: String,
    /// 发送用户的文字回复
    pub reply: tokio::sync::oneshot::Sender<String>,
}
