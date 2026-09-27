use crate::client::catalog::ProviderProfile;
use crate::client::catalog::ThinkingEffort;
use omini_domain::agent_run::AgentRunSnapshot;
use omini_domain::subagents::{AgentDraft, AgentRecord, AgentSourceKind};
pub use omini_domain::task::{TaskChangedEvent, TaskOutputDelta};
use omini_model::message::{ToolResultBlock, ToolUseBlock};
use omini_protocol::HistoryItem;
pub use omini_runtime_contract::thread_domain::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Runtime → UI 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeToUiEvent {
    AgentRunChanged(AgentRunSnapshot),
    /// 用户输入已提交，运行时开始处理
    RunStarted,
    /// Runtime 注入了一条用户消息，UI 需要显示到消息区
    UserMessageInjected {
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    AgentTaskUserMessageQueued {
        task_id: String,
        thread_id: String,
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    AgentTaskMessageQueued {
        task_id: String,
        item: HistoryItem,
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
    /// 当前会话 token usage 状态已变更。
    UsageChanged(ThreadUsageSnapshot),
    /// 当前会话累计 token usage 已变更，但当前 context used 不应同步。
    UsageTotalsChanged {
        total_tokens: i64,
        total_cached_tokens: i64,
    },
    /// TUI 连接已有 thread 后从 server status 同步当前 query 计时器。
    RuntimeStatusSynced {
        status: omini_protocol::ThreadRuntimeStatus,
        restore_pending_pauses: bool,
    },
    /// 当前线程快照已同步。
    ThreadSnapshot {
        thread_id: Option<String>,
        messages: Vec<HistoryItem>,
        agent_tasks: Vec<AgentTaskSnapshot>,
        usage: ThreadUsageSnapshot,
    },

    /// 线程标题变更（TUI 头部栏显示用）
    ThreadTitleChanged {
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
    /// 通用后台任务状态变更；呈现由 Client 决定。
    TaskChanged(TaskChangedEvent),
    /// 后台 Bash 的 stdout/stderr 增量；呈现由 Client 决定。
    TaskOutputDelta(TaskOutputDelta),
    /// 当前 thread 开始 LLM 压缩摘要。
    CompactSummaryStarted(CompactEvent),
    /// 当前 thread 正在流式输出压缩摘要。
    CompactSummaryDelta(CompactSummaryDeltaEvent),
    /// 当前 thread 完成 LLM 压缩摘要。
    CompactSummaryFinished(CompactSummaryFinishedEvent),
    /// 当前 thread LLM 压缩摘要失败。
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
    use crate::app::event::ActiveProfile;
    use crate::app::event::CommandSummary;
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

    use crate::app::event::{ActiveProfile, CommandKind, CommandSummary, RuntimeToUiEvent};
    use omini_protocol::HistoryItem;
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
                "type": "user_input",
                "entry": {
                    "intent": {"type": "message"},
                    "parts": [{"type": "text", "text": "@worker hello"}]
                }
            }
        });
        let decoded: RuntimeToUiEvent =
            serde_json::from_value(value.clone()).expect("deserialize typed user input");
        assert!(matches!(
            &decoded,
            RuntimeToUiEvent::UserMessageInjected {
                item: HistoryItem::UserInput(input),
                client_echo_id: None,
            } if matches!(input.parts.as_slice(), [omini_domain::input::InputPart::Text { text }] if text == "@worker hello")
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize typed user input"),
            value
        );
        let value = json!({
            "type": "user_message_injected",
            "client_echo_id": "echo-1",
            "item": {
                "type": "user_input",
                "entry": {
                    "intent": {"type": "message"},
                    "parts": [],
                    "attachments": [{
                        "attachment_id": "attachment-1",
                        "mime_type": "image/png",
                        "size": 3,
                        "name": "image.png"
                    }]
                }
            }
        });
        let decoded: RuntimeToUiEvent =
            serde_json::from_value(value.clone()).expect("deserialize attachment user input");
        assert!(matches!(
            &decoded,
            RuntimeToUiEvent::UserMessageInjected {
                item: HistoryItem::UserInput(input),
                client_echo_id,
            } if input.attachments.len() == 1
                && client_echo_id.as_deref() == Some("echo-1")
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize attachment user input"),
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
    /// 线程选择：列出项目下所有线程
    ThreadSelection { threads: Vec<ThreadSummary> },
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
