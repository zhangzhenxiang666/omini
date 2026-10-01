use crate::thread_domain::{
    ActiveProfile, AgentTaskEventEnvelope, CompactEvent, CompactSummaryDeltaEvent,
    CompactSummaryFailedEvent, CompactSummaryFinishedEvent, Notification, PlanApprovalAction,
    SubmittedPlan, ThreadUsageSnapshot, ToolPauseRequest,
};
use omini_domain::agent_run::AgentRunSnapshot;
use omini_domain::config::ThinkingEffort;
use omini_domain::subagents::AgentRecord;
use omini_domain::task::{TaskChangedEvent, TaskOutputDelta};
use omini_model::message::{Message, ToolResultBlock, ToolUseBlock};
use serde::{Deserialize, Serialize};

/// runtime 发往 server/facade 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeToServerEvent {
    AgentRunChanged(AgentRunSnapshot),
    RunStarted,
    RunFinished,
    Notification(Notification),
    ModelChanged {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
        context_window: Option<u32>,
    },
    UsageChanged(ThreadUsageSnapshot),
    UsageTotalsChanged {
        total_tokens: i64,
        total_cached_tokens: i64,
    },
    ActiveProfileChanged(#[serde(with = "serde_runtime_event_payload::profile")] ActiveProfile),
    AgentManagementUpdated {
        records: Vec<AgentRecord>,
    },
    TurnStarted,
    TurnEnded,
    ThinkingDelta(#[serde(with = "serde_runtime_event_payload::delta")] String),
    TextDelta(#[serde(with = "serde_runtime_event_payload::delta")] String),
    ProposedPlanDelta(#[serde(with = "serde_runtime_event_payload::delta")] String),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    TaskChanged(TaskChangedEvent),
    TaskOutputDelta(TaskOutputDelta),
    CompactSummaryStarted(CompactEvent),
    CompactSummaryDelta(CompactSummaryDeltaEvent),
    CompactSummaryFinished(CompactSummaryFinishedEvent),
    CompactSummaryFailed(CompactSummaryFailedEvent),
    ToolPauseRequested(ToolPauseRequest),
    PlanSubmitted(SubmittedPlan),
    PlanApprovalResolved {
        plan_id: String,
        action: PlanApprovalAction,
    },
    /// 计划批准后提交给 LLM 上下文的消息，由 server 决定如何投影到用户历史。
    PlanApprovalAccepted {
        message: Message,
    },
    /// server 端 fork 出新 ThreadSession 后，作为原 thread 推送给客户端的
    /// 外部线程切换通知。承载在普通 runtime 通道上，以便 ws 文本帧能直接编码为
    /// `TypedRuntimeEvent::ThreadSwitched`。
    ThreadSwitched {
        from: String,
        to: String,
    },
    AgentTaskEvent(AgentTaskEventEnvelope),
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
    use crate::thread_domain::ActiveProfile;
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
