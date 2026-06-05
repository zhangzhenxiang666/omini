use crate::persistence::SessionRecord;
use crate::types::config::ThinkingEffort;
use crate::types::display::{HistoryItem, UserDraft};
use crate::types::message::{Message, ToolResultBlock, ToolUseBlock};
use crate::types::subagents::{AgentDraft, AgentRecord, AgentSourceKind};
use crate::types::usage::Usage;
pub use omini_domain::events::*;
use serde::{Deserialize, Serialize};

// ===========================================================================
// 第一层：Server → Runtime 的事件
// ===========================================================================

/// Server/facade → Runtime 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToRuntimeEvent {
    /// 用户取消当前正在运行的对话
    CancelRun,
    /// 用户发送一条消息给 runtime
    SendMessage {
        draft: UserDraft,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    /// 用户请求压缩当前会话上下文。
    CompactContext { instructions: Option<String> },
    /// 用户请求调整 thinking effort。
    SetThinkingEffort(ThinkingEffort),
    /// 用户请求调整 thinking 块显示偏好。
    SetThinkingDisplay { show: Option<bool> },
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
    /// 外部 server 已加载会话快照，要求 runtime hydrate 当前会话状态。
    HydrateSessionSnapshot { snapshot: LoadedSession },
    /// server 正在回收该 session runtime。
    CloseRuntime,
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
/// 3. 翻译为 `RuntimeToServerEvent` 转发给 server
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
    /// Runtime 收到后转发 `RuntimeToServerEvent::TurnEnded` 给 server。
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

// ===========================================================================
// 第三层：Runtime → Server 的事件
// ===========================================================================

/// Runtime → Server/facade 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeToServerEvent {
    /// 用户输入已提交，运行时开始处理
    RunStarted,
    /// Runtime 注入了一条用户消息，客户端需要显示到消息区。
    UserMessageInjected {
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    /// 所有轮次完成，运行结束
    RunFinished,

    /// 运行时产生的通知信息（显示在消息区，但不作为对话消息）
    Notification(Notification),

    /// 模型已切换（客户端更新状态栏用）。
    ModelChanged {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
        context_window: Option<u32>,
    },
    /// thinking 块显示偏好已变更。
    ThinkingDisplayChanged { show: bool },
    /// 当前会话 token usage 状态已变更。
    UsageChanged(SessionUsageSnapshot),
    /// 当前会话累计 token usage 已变更，但当前 context used 不应同步。
    UsageTotalsChanged {
        total_tokens: i64,
        total_cached_tokens: i64,
    },
    /// 当前会话快照已同步。
    SessionSnapshot {
        session_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<SubagentSnapshot>,
        usage: SessionUsageSnapshot,
    },

    /// 会话标题变更（客户端头部栏显示用）。
    SessionTitleChanged { title: Option<String> },
    /// 当前 profile 已变更
    ActiveProfileChanged(#[serde(with = "serde_runtime_event_payload::profile")] ActiveProfile),
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
    /// 计划已提交，客户端应打开计划审批抽屉。
    PlanSubmitted(SubmittedPlan),
    /// 计划审批已被任一客户端处理，所有客户端都应关闭对应抽屉。
    PlanApprovalResolved {
        plan_id: String,
        action: PlanApprovalAction,
    },

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
}

impl RuntimeToServerEvent {
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
}

#[cfg(test)]
mod tests {
    use crate::types::display::{DisplayMessage, HistoryItem};
    use crate::types::events::{ActiveProfile, RuntimeToServerEvent};
    use crate::types::message::{ContentBlock, Message, Role};
    use serde_json::json;

    #[test]
    fn runtime_event_newtype_payloads_serialize_as_tagged_maps() {
        let value = serde_json::to_value(RuntimeToServerEvent::ThinkingDelta("思考".to_string()))
            .expect("serialize thinking delta");
        assert_eq!(value, json!({"type": "thinking_delta", "delta": "思考"}));
        let decoded: RuntimeToServerEvent =
            serde_json::from_value(value).expect("deserialize thinking delta");
        assert!(matches!(decoded, RuntimeToServerEvent::ThinkingDelta(delta) if delta == "思考"));

        let value = serde_json::to_value(RuntimeToServerEvent::UserMessageInjected {
            item: HistoryItem::Display(DisplayMessage {
                role: Role::User,
                text: "@worker hello".to_string(),
                mentions: Vec::new(),
            }),
            client_echo_id: None,
        })
        .expect("serialize display user message");
        assert_eq!(
            value,
            json!({
                "type": "user_message_injected",
                "item": {
                    "type": "display",
                    "role": "user",
                    "text": "@worker hello"
                }
            })
        );
        let value = serde_json::to_value(RuntimeToServerEvent::UserMessageInjected {
            item: HistoryItem::Message(Message::new(
                Role::User,
                vec![ContentBlock::from_base64_image(
                    "image/png".to_string(),
                    "abc".to_string(),
                )],
            )),
            client_echo_id: Some("echo-1".to_string()),
        })
        .expect("serialize image user message");
        assert_eq!(
            value,
            json!({
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
            })
        );

        let value = serde_json::to_value(RuntimeToServerEvent::ActiveProfileChanged(
            ActiveProfile::Plan,
        ))
        .expect("serialize profile");
        assert_eq!(
            value,
            json!({"type": "active_profile_changed", "profile": "plan"})
        );
    }
}
