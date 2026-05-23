use crate::config::ProviderProfile;
use crate::config::ThinkingEffort;
use crate::display::{DisplayMessage, DisplayPlan, HistoryItem, UserDraft};
use crate::message::{Message, ToolResultBlock, ToolUseBlock};
use crate::subagents::{AgentDraft, AgentRecord, AgentSourceKind, AgentSummary};
use crate::usage::Usage;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveProfile {
    #[default]
    Main,
    Plan,
}

impl ActiveProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Plan => "plan",
        }
    }
}

impl std::fmt::Display for ActiveProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ===========================================================================
// 第一层：UI → Runtime 的事件
// ===========================================================================

/// UI → Runtime 的事件。
#[derive(Debug)]
pub enum UiToRuntimeEvent {
    /// 用户取消当前正在运行的对话
    CancelRun,
    /// 用户发送一条消息给 runtime
    SendMessage(UserDraft),
    /// 用户执行一条命令
    SendCommand(String),
    /// 用户切换当前 active profile
    ToggleActiveProfile,
    /// 用户发送一条消息插入正在运行的 query，在下一轮 LLM 调用前生效
    InterveneMessage(UserDraft),
    /// 用户在模型选择页中确认选择
    ModelSelected {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
    },
    /// 用户在会话选择页中确认选择
    SessionSelected { session_id: String },
    /// 用户请求保存 agent
    AgentSaveRequested {
        source_kind: AgentSourceKind,
        original_path: Option<std::path::PathBuf>,
        draft: AgentDraft,
    },
    /// 用户请求删除 agent
    AgentDeleteRequested { path: std::path::PathBuf },
    /// 用户请求由 LLM 生成 agent
    AgentGenerateRequested {
        source_kind: AgentSourceKind,
        description: String,
        tools: Vec<String>,
        disallow_tools: Vec<String>,
        model: Option<String>,
    },
    /// 用户响应工具暂停请求
    ResolveToolPause {
        tool_use_id: String,
        response: ToolPauseResponse,
    },
    /// 用户响应计划审批抽屉
    ResolvePlanApproval {
        plan_id: String,
        action: PlanApprovalAction,
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
    /// 模型提交了计划，runtime 已完成持久化
    PlanSubmitted(SubmittedPlan),
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

// ===========================================================================
// 第三层：Runtime → UI 的事件
// ===========================================================================

/// Runtime → UI 的事件。
#[derive(Debug)]
pub enum RuntimeToUiEvent {
    /// 用户输入已提交，运行时开始处理
    RunStarted,
    /// Runtime 注入了一条用户消息，UI 需要显示到消息区
    UserMessageInjected(HistoryItem),
    /// 所有轮次完成，运行结束
    RunFinished,

    /// 请求关闭整个程序
    Shutdown,

    /// 命令产生的提示信息（显示在消息区，但不作为对话消息）
    CommandNotice(String),
    /// 运行时产生的警告信息（显示在消息区，但不作为对话消息）
    Warning(String),

    /// 模型已切换（TUI 更新状态栏用）
    ModelChanged {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
        context_window: Option<u32>,
    },
    /// 当前会话 token usage 状态已变更。
    UsageChanged(SessionUsageSnapshot),
    /// 当前会话累计 token usage 已变更，但当前 context used 不应同步。
    UsageTotalsChanged {
        total_tokens: i64,
        total_cached_tokens: i64,
    },
    /// 会话已切换
    SessionChanged {
        session_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<SubagentSnapshot>,
        usage: SessionUsageSnapshot,
    },

    /// 会话标题变更（TUI 头部栏显示用）
    SessionTitleChanged { title: Option<String> },
    /// 当前 profile 已变更
    ActiveProfileChanged(ActiveProfile),
    /// 需要 TUI 弹出交互选择页
    InteractionRequest(InteractionRequest),
    /// 需要 TUI 打开帮助抽屉
    ShowHelpDrawer(Vec<CommandSummary>),

    /// Runtime 启动时推送命令列表（供自动补全使用）
    CommandList(Vec<CommandSummary>),
    /// Runtime 启动时推送 subagent 列表（供 @ mention 自动补全使用）
    AgentList(Vec<AgentSummary>),
    /// Runtime 刷新 `/agents` 面板数据
    AgentManagementUpdated { records: Vec<AgentRecord> },
    /// LLM 已生成 agent 草稿，供 `/agents` 面板预览和保存
    AgentGenerated {
        source_kind: AgentSourceKind,
        draft: AgentDraft,
    },
    /// LLM 生成 agent 失败，供 `/agents` 面板恢复输入态并显示错误。
    AgentGenerateFailed { message: String },

    /// 新一轮 LLM 调用开始
    TurnStarted,
    /// 当前轮 LLM 调用结束（所有 content block 已收齐）
    TurnEnded,

    /// thinking 块流式增量
    ThinkingDelta(String),
    /// text 块流式增量
    TextDelta(String),
    /// plan mode 中 `<proposed_plan>` 块的流式增量
    ProposedPlanDelta(String),

    /// LLM 发起了工具调用
    ToolUse(ToolUseBlock),
    /// 工具执行完成，产出结果
    ToolResult(ToolResultBlock),
    /// 当前 session 开始 LLM 压缩摘要。
    CompactSummaryStarted(CompactEvent),
    /// 当前 session 正在流式输出压缩摘要。
    CompactSummaryDelta(CompactSummaryDeltaEvent),
    /// 当前 session 完成 LLM 压缩摘要。
    CompactSummaryFinished(CompactSummaryFinishedEvent),
    /// 当前 session LLM 压缩摘要失败。
    CompactSummaryFailed(CompactSummaryFailedEvent),

    /// 工具需要暂停等待用户授权或输入
    ToolPauseRequested(ToolPauseRequest),
    /// 计划已提交，TUI 应打开计划审批抽屉
    PlanSubmitted(SubmittedPlan),

    /// 子 agent 创建并开始运行。
    SubagentStarted(SubagentStartedEvent),
    /// 子 agent 产生了一条完整消息。
    SubagentMessageProduced(SubagentMessageEvent),
    /// 子 agent 请求工具调用。
    SubagentToolUse(SubagentToolUseEvent),
    /// 子 agent 工具执行完成。
    SubagentToolResult(SubagentToolResultEvent),
    /// 子 agent 运行结束。
    SubagentFinished(SubagentFinishedEvent),

    /// 运行时出错
    Error(String),
}

#[derive(Debug, Clone)]
pub struct SubagentStartedEvent {
    pub session_id: String,
    pub parent_session_id: String,
    pub spawn_tool_use_id: String,
    pub agent_label: String,
}

#[derive(Debug, Clone)]
pub struct SubagentSnapshot {
    pub session_id: String,
    pub parent_session_id: String,
    pub spawn_tool_use_id: String,
    pub agent_label: String,
    pub status: SubagentStatus,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct SubagentMessageEvent {
    pub session_id: String,
    pub message: Message,
}

#[derive(Debug, Clone)]
pub struct SubagentToolUseEvent {
    pub session_id: String,
    pub tool_use: ToolUseBlock,
}

#[derive(Debug, Clone)]
pub struct SubagentToolResultEvent {
    pub session_id: String,
    pub tool_result: ToolResultBlock,
}

#[derive(Debug, Clone)]
pub struct SubagentFinishedEvent {
    pub session_id: String,
    pub status: SubagentStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactTrigger {
    Auto,
    Manual,
}

impl CompactTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

impl std::fmt::Display for CompactTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct CompactEvent {
    pub trigger: CompactTrigger,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompactSummaryDeltaEvent {
    pub trigger: CompactTrigger,
    pub delta: String,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompactShrinkFinishedEvent {
    pub trigger: CompactTrigger,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub before_messages: usize,
    pub after_messages: usize,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompactShrinkFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompactSummaryFinishedEvent {
    pub trigger: CompactTrigger,
    pub summary: String,
    pub after_tokens: usize,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompactSummaryFailedEvent {
    pub trigger: CompactTrigger,
    pub message: String,
    pub session_id: Option<String>,
    pub agent_label: Option<String>,
}

pub type SubmittedPlan = DisplayPlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanApprovalAction {
    Approve,
    ApproveAndCompact,
    ContinueDiscussing,
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
    /// Agent 管理：列出、查看、创建、编辑、删除 subagent
    AgentManagement {
        records: Vec<AgentRecord>,
        providers: HashMap<String, ProviderProfile>,
        current_provider: String,
        current_model: String,
    },
}

/// 会话摘要（供选择页展示）。
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionUsageSnapshot {
    pub current_context_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    pub context_window: Option<u32>,
}

/// 命令摘要（供自动补全 / 帮助展示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSummary {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub sort_weight: i32,
    pub kind: CommandKind,
    /// true = 需要额外参数，选中后只补全命令名+空格
    /// false = 无参数，选中后直接执行
    pub has_args: bool,
    pub args_description: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Builtin,
    Skill,
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
    /// 注入一条 LLM 消息并用另一条消息作为 UI/数据库回显。
    InjectUserQuery {
        llm_message: Message,
        display_message: DisplayMessage,
    },
    /// 不新增用户消息，直接基于当前历史继续启动 query。
    ContinueQuery,
    /// 复用已有 Runtime → UI 事件表达非命令专属的生命周期变更。
    Emit(Box<RuntimeToUiEvent>),
}

impl CommandEffect {
    pub fn emit(event: RuntimeToUiEvent) -> Self {
        Self::Emit(Box::new(event))
    }

    pub fn inject_user_query(llm_message: Message, display_message: DisplayMessage) -> Self {
        Self::InjectUserQuery {
            llm_message,
            display_message,
        }
    }
}

// ===========================================================================
// 共享类型
// ===========================================================================

/// 工具暂停请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolPauseRequest {
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_tool_use_id: Option<String>,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_source: Option<PermissionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
    pub kind: ToolPauseKind,
}

/// Explains which configured permission rule made a decision visible to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSource {
    pub decision: String,
    pub source: String,
    pub rule: String,
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
    Permission {
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    UserInput {
        value: Value,
    },
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
    Search(SearchPermissionPreview),
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
pub struct SearchPermissionPreview {
    pub query: String,
    pub mode: String,
    pub path: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_response_deserializes_without_note() {
        let response: ToolPauseResponse = serde_json::from_value(json!({
            "type": "permission",
            "approved": false,
        }))
        .unwrap();

        assert_eq!(
            response,
            ToolPauseResponse::Permission {
                approved: false,
                note: None,
            }
        );
    }
}
