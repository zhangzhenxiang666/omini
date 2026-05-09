use crate::types::message::{ToolResultBlock, ToolUseBlock};
use serde_json::Value;
use std::collections::HashMap;

/// UI → Agent runtime 的请求。
///
/// 当用户需要向 runtime 发送操作指令时（如取消运行、发送消息等），
/// UI 通过独立的 channel 发送此指令。
#[derive(Debug)]
pub enum UiRequest {
    /// 用户取消当前正在运行的对话
    CancelRun,
    /// 用户发送一条消息给 runtime
    SendMessage(String),
}

/// Agent runtime → UI 的事件。
#[derive(Debug)]
pub enum RuntimeEvent {
    /// 用户输入已提交，运行时开始处理
    RunStarted,
    /// 所有轮次完成，运行结束
    RunFinished,

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
    /// LLM 向用户提问（如 ask_user 工具）
    UserConfirmation(UserConfirmation),

    /// 运行时出错
    Error(String),
}

/// 工具权限请求。
/// 当一个工具需要用户授权才能执行时，runtime 会发送此事件。
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
/// 当 LLM 调用 ask_user 等工具需要用户输入时，runtime 会发送此事件。
/// UI 展示输入框后，通过 `reply` 发送用户的文字回复。
#[derive(Debug)]
pub struct UserConfirmation {
    /// LLM 提出的问题
    pub question: String,
    /// 发送用户的文字回复
    pub reply: tokio::sync::oneshot::Sender<String>,
}
