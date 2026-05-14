use crate::types::config::ProviderProfile;
use crate::types::config::ThinkingEffort;
use crate::types::message::{Message, ToolResultBlock, ToolUseBlock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ===========================================================================
// 第一层：UI → Runtime 的事件
// ===========================================================================

/// UI → Runtime 的事件。
#[derive(Debug)]
pub enum UiToRuntimeEvent {
    /// 用户取消当前正在运行的对话
    CancelRun,
    /// 用户发送一条消息给 runtime
    SendMessage(Message),
    /// 用户执行一条命令
    SendCommand(String),
    /// 用户发送一条消息插入正在运行的 query，在下一轮 LLM 调用前生效
    InterveneMessage(Message),
    /// 用户在模型选择页中确认选择
    ModelSelected {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
    },
    /// 用户在会话选择页中确认选择
    SessionSelected { session_id: String },
    /// 用户响应工具暂停请求
    ResolveToolPause {
        tool_use_id: String,
        response: ToolPauseResponse,
    },
}

// ===========================================================================
// 第二层：Engine → Runtime 的内部事件
// ===========================================================================

/// Engine → Runtime 的内部事件。
///
/// Runtime 消费此事件后负责：
/// 1. 更新内部 `messages` 状态
/// 2. 增量持久化
/// 3. 翻译为 `RuntimeToUiEvent` 转发给 UI
#[derive(Debug)]
pub enum EngineToRuntimeEvent {
    /// 一条 User Message 已进入引擎消息历史，需要按当前位置持久化。
    UserMessageProduced(Message),

    /// 引擎完成一轮流式输出，产出一条完整的 Assistant Message。
    MessageProduced(Message),

    /// 引擎收集完所有工具结果，打包成一条 User Message。
    ToolResultsProduced(Message),

    /// 当前轮完整结束（助理消息 + 工具结果均已产出）。
    /// Runtime 收到后转发 `RuntimeToUiEvent::TurnEnded` 给 UI。
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
    ToolPauseRequested(ToolPauseRequest),

    /// 引擎出错
    Error(String),
}

// ===========================================================================
// 第三层：Runtime → UI 的事件
// ===========================================================================

/// Runtime → UI 的事件。
#[derive(Debug)]
pub enum RuntimeToUiEvent {
    /// 用户输入已提交，运行时开始处理
    RunStarted,
    /// Runtime 注入了一条用户消息，UI 需要显示到消息区
    UserMessageInjected(Message),
    /// 所有轮次完成，运行结束
    RunFinished,

    /// 请求关闭整个程序
    Shutdown,

    /// 命令产生的提示信息（显示在消息区，但不作为对话消息）
    CommandNotice(String),

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

    /// 工具需要暂停等待用户授权或输入
    ToolPauseRequested(ToolPauseRequest),

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
    Ok(Vec<CommandEffect>),
    Error(String),
}

/// 命令执行后需要 runtime 统一应用的语义化效果。
#[derive(Debug)]
pub enum CommandEffect {
    /// 无状态提示信息，仅用于 UI 展示。
    Notice(String),
    /// 请求 UI 打开一个交互面板。
    ShowInteraction(InteractionRequest),
    /// 注入一条用户消息并立即启动 query。
    InjectUserMessage(Message),
    /// 复用已有 Runtime → UI 事件表达非命令专属的生命周期变更。
    Emit(RuntimeToUiEvent),
}

// ===========================================================================
// 共享类型
// ===========================================================================

/// 工具暂停请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolPauseRequest {
    pub tool_use_id: String,
    pub tool_name: String,
    pub kind: ToolPauseKind,
}

/// 工具暂停类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPauseKind {
    Permission(PermissionPreview),
    UserInput(UserInputPreview),
}

/// 工具暂停响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPauseResponse {
    Permission { approved: bool },
    UserInput { value: Value },
    Cancelled,
}

/// 权限审批预览。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PermissionPreview {
    Bash(BashPermissionPreview),
    Edit(EditPermissionPreview),
    Write(EditPermissionPreview),
    Read(ReadPermissionPreview),
    Custom {
        tool_name: String,
        payload: serde_json::Map<String, Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BashPermissionPreview {
    pub command: String,
    pub description: Option<String>,
    pub workdir: Option<String>,
    pub timeout: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadPermissionPreview {
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditPermissionPreview {
    pub summary: String,
    pub path: String,
    pub replacement_count: usize,
    pub replace_all: bool,
    pub start_lines: Vec<usize>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInputPreview {
    pub questions: Vec<UserInputQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}
