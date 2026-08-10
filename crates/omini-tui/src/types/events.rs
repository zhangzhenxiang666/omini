use crate::types::config::ProviderProfile;
use crate::types::config::ThinkingEffort;
use omini_domain::display::{DisplayMessage, HistoryItem, UserDraft};
pub use omini_domain::events::*;
use omini_domain::message::{Message, ToolResultBlock, ToolUseBlock};
use omini_domain::subagents::{AgentDraft, AgentRecord, AgentSourceKind};
use omini_domain::usage::Usage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ===========================================================================
// 第一层：UI → Runtime 的事件
// ===========================================================================

/// UI → Runtime 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiToRuntimeEvent {
    /// 用户取消当前正在运行的对话
    CancelRun,
    /// 用户发送一条消息给 runtime
    SendMessage {
        draft: UserDraft,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    /// 用户执行一条命令
    SendCommand(UserDraft),
    /// 用户请求压缩当前会话上下文。
    CompactContext { instructions: Option<String> },
    /// 用户请求调整 thinking effort。
    SetThinkingEffort(ThinkingEffort),
    /// 用户请求调整 thinking 块显示偏好。
    SetThinkingDisplay { show: Option<bool> },
    /// 用户请求打开帮助抽屉。
    ShowHelp,
    /// 用户切换当前 active profile
    ToggleActiveProfile,
    /// 用户显式设置当前 active profile
    SetActiveProfile(ActiveProfile),
    /// 用户发送一条消息插入正在运行的 query，在下一轮 LLM 调用前生效
    InterveneMessage {
        draft: UserDraft,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    /// 用户在模型选择页中确认选择
    ModelSelected {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
    },
    /// server 已完成 agent 文件变更，要求 runtime 刷新当前会话可用的 subagent 能力。
    SubagentRegistryChanged,
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
    ToolPauseRequested(Box<ToolPauseRequest>),
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

    /// 引擎出错
    Error(String),
    /// 引擎运行时警告
    Warning(String),
}

// ===========================================================================
// 第三层：Runtime → UI 的事件
// ===========================================================================

/// Runtime → UI 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeToUiEvent {
    /// 用户输入已提交，运行时开始处理
    RunStarted,
    /// Runtime 注入了一条用户消息，UI 需要显示到消息区
    UserMessageInjected {
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    /// 所有轮次完成，运行结束
    RunFinished,

    /// 请求关闭整个程序
    Shutdown,

    /// 运行时产生的通知信息（显示在消息区，但不作为对话消息）
    Notification(Notification),

    /// 模型已切换（TUI 更新状态栏用）
    ModelChanged {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
        context_window: Option<u32>,
    },
    /// thinking 块显示偏好已变更。
    ThinkingDisplayChanged {
        show: bool,
    },
    /// 当前会话 token usage 状态已变更。
    UsageChanged(SessionUsageSnapshot),
    /// 当前会话累计 token usage 已变更，但当前 context used 不应同步。
    UsageTotalsChanged {
        total_tokens: i64,
        total_cached_tokens: i64,
    },
    /// TUI 连接已有 session 后从 server status 同步当前 query 计时器。
    RuntimeStatusSynced {
        status: omini_protocol::SessionRuntimeStatus,
        restore_pending_pauses: bool,
    },
    /// 当前会话快照已同步。
    SessionSnapshot {
        session_id: Option<String>,
        messages: Vec<HistoryItem>,
        agent_tasks: Vec<AgentTaskSnapshot>,
        usage: SessionUsageSnapshot,
    },

    /// 会话标题变更（TUI 头部栏显示用）
    SessionTitleChanged {
        title: Option<String>,
    },
    /// 当前 profile 已变更
    ActiveProfileChanged(#[serde(with = "serde_runtime_event_payload::profile")] ActiveProfile),
    /// 需要 TUI 弹出交互选择页
    InteractionRequest(InteractionRequest),
    /// 需要 TUI 打开帮助抽屉
    ShowHelpDrawer(#[serde(with = "serde_runtime_event_payload::commands")] Vec<CommandSummary>),

    /// Runtime 启动时推送命令列表（供自动补全使用）
    CommandList(#[serde(with = "serde_runtime_event_payload::commands")] Vec<CommandSummary>),
    /// Runtime 刷新 `/agents` 面板数据
    AgentManagementUpdated {
        records: Vec<AgentRecord>,
    },
    /// LLM 已生成 agent 草稿，供 `/agents` 面板预览和保存
    AgentGenerated {
        source_kind: AgentSourceKind,
        draft: AgentDraft,
    },
    /// LLM 生成 agent 失败，供 `/agents` 面板恢复输入态并显示错误。
    AgentGenerateFailed {
        message: String,
    },

    /// 新一轮 LLM 调用开始
    TurnStarted,
    /// 当前轮 LLM 调用结束（所有 content block 已收齐）
    TurnEnded,

    /// git 分支已变化
    GitBranchChanged {
        branch: Option<String>,
    },

    ThinkingDelta(#[serde(with = "serde_runtime_event_payload::delta")] String),
    /// text 块流式增量
    TextDelta(#[serde(with = "serde_runtime_event_payload::delta")] String),
    /// plan mode 中 `<proposed_plan>` 块的流式增量
    ProposedPlanDelta(#[serde(with = "serde_runtime_event_payload::delta")] String),

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
    /// 计划审批已被任一客户端处理，所有客户端都应关闭对应抽屉。
    PlanApprovalResolved {
        plan_id: String,
        action: PlanApprovalAction,
    },

    /// 单个 Agent task 的统一生命周期与流式事件。
    AgentTaskEvent(AgentTaskEventEnvelope),
}

impl RuntimeToUiEvent {
    pub fn notice(message: impl Into<String>) -> Self {
        Self::Notification(Notification::info(message))
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::Notification(Notification::warning(message))
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Notification(Notification::error(message))
    }
}

mod serde_runtime_event_payload {
    use crate::types::events::ActiveProfile;
    use crate::types::events::CommandSummary;
    use serde::Deserialize;
    use serde::Serializer;
    use serde::ser::SerializeStruct;

    pub mod delta {
        use super::*;

        pub fn serialize<S>(delta: &String, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut state = serializer.serialize_struct("DeltaPayload", 1)?;
            state.serialize_field("delta", delta)?;
            state.end()
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<String, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct DeltaPayload {
                delta: String,
            }

            Ok(DeltaPayload::deserialize(deserializer)?.delta)
        }
    }

    pub mod profile {
        use super::*;

        pub fn serialize<S>(profile: &ActiveProfile, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut state = serializer.serialize_struct("ProfilePayload", 1)?;
            state.serialize_field("profile", profile)?;
            state.end()
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<ActiveProfile, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct ProfilePayload {
                profile: ActiveProfile,
            }

            Ok(ProfilePayload::deserialize(deserializer)?.profile)
        }
    }

    pub mod commands {
        use super::*;

        pub fn serialize<S>(commands: &[CommandSummary], serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut state = serializer.serialize_struct("CommandsPayload", 1)?;
            state.serialize_field("commands", commands)?;
            state.end()
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<CommandSummary>, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct CommandsPayload {
                commands: Vec<CommandSummary>,
            }

            Ok(CommandsPayload::deserialize(deserializer)?.commands)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::types::events::{ActiveProfile, CommandKind, CommandSummary, RuntimeToUiEvent};
    use omini_domain::display::HistoryItem;
    use serde_json::json;

    #[test]
    fn runtime_event_newtype_payloads_round_trip_as_tagged_maps() {
        let value = json!({"type": "thinking_delta", "delta": "思考"});
        let decoded: RuntimeToUiEvent =
            serde_json::from_value(value.clone()).expect("deserialize thinking delta");
        assert!(matches!(&decoded, RuntimeToUiEvent::ThinkingDelta(delta) if delta == "思考"));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize thinking delta"),
            value
        );

        let value = json!({
            "type": "user_message_injected",
            "item": {
                "type": "display",
                "role": "user",
                "text": "@worker hello"
            }
        });
        let decoded: RuntimeToUiEvent =
            serde_json::from_value(value.clone()).expect("deserialize display user message");
        assert!(matches!(
            &decoded,
            RuntimeToUiEvent::UserMessageInjected {
                item: HistoryItem::Display(display),
                client_echo_id: None,
            } if display.text == "@worker hello"
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize display user message"),
            value
        );
        let value = json!({
            "type": "user_message_injected",
            "client_echo_id": "echo-1",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": "abc"
                    }
                }]
            }
        });
        let decoded: RuntimeToUiEvent =
            serde_json::from_value(value.clone()).expect("deserialize image user message");
        assert!(matches!(
            &decoded,
            RuntimeToUiEvent::UserMessageInjected {
                item: HistoryItem::Message(message),
                client_echo_id,
            } if message.content.first().is_some_and(|block| block.is_image())
                && client_echo_id.as_deref() == Some("echo-1")
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize image user message"),
            value
        );

        let value = json!({"type": "active_profile_changed", "profile": "plan"});
        let decoded: RuntimeToUiEvent =
            serde_json::from_value(value.clone()).expect("deserialize profile");
        assert!(matches!(
            &decoded,
            RuntimeToUiEvent::ActiveProfileChanged(ActiveProfile::Plan)
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize profile"),
            value
        );

        let command = CommandSummary {
            name: "help".to_string(),
            aliases: vec!["?".to_string()],
            description: "Show help.".to_string(),
            sort_weight: 0,
            has_args: false,
            args_description: None,
            kind: CommandKind::Builtin,
        };
        let value = serde_json::to_value(RuntimeToUiEvent::CommandList(vec![command]))
            .expect("serialize command list");
        assert_eq!(
            value,
            json!({
                "type": "command_list",
                "commands": [{
                    "name": "help",
                    "aliases": ["?"],
                    "description": "Show help.",
                    "sort_weight": 0,
                    "has_args": false,
                    "args_description": null,
                    "kind": "builtin"
                }]
            })
        );
    }
}

// ===========================================================================
// 命令系统相关类型
// ===========================================================================

/// 交互请求（Runtime → TUI，触发选择页）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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

/// 命令摘要（供自动补全 / 帮助展示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSummary {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub sort_weight: i32,
    /// true = 需要额外参数，选中后只补全命令名+空格
    /// false = 无参数，选中后直接执行
    pub has_args: bool,
    pub args_description: Option<String>,
    pub kind: CommandKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// 无状态通知信息，仅用于 UI 展示。
    Notification(Notification),
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
    pub fn notification(notification: Notification) -> Self {
        Self::Notification(notification)
    }

    pub fn notice(message: impl Into<String>) -> Self {
        Self::Notification(Notification::info(message))
    }

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
