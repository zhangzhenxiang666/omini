use omini_domain::config::ThinkingEffort;
use omini_domain::display::{HistoryItem, UserDraft};
use omini_domain::events::{
    ActiveProfile, CompactEvent, CompactSummaryDeltaEvent, CompactSummaryFailedEvent,
    CompactSummaryFinishedEvent, LoadedSession, Notification, PlanApprovalAction,
    SessionUsageSnapshot, SubagentFinishedEvent, SubagentMessageEvent, SubagentSnapshot,
    SubagentStartedEvent, SubagentToolResultEvent, SubagentToolUseEvent, SubmittedPlan,
    ToolPauseRequest, ToolPauseResponse,
};
use omini_domain::message::{ToolResultBlock, ToolUseBlock};
use omini_domain::subagents::AgentRecord;
use serde::{Deserialize, Serialize};

/// server/facade 发往 runtime 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerToRuntimeEvent {
    CancelRun,
    SendMessage {
        draft: UserDraft,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    CompactContext {
        instructions: Option<String>,
    },
    SetThinkingEffort(ThinkingEffort),
    ToggleActiveProfile,
    SetActiveProfile(ActiveProfile),
    InterveneMessage {
        draft: UserDraft,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    ModelSelected {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
    },
    HydrateSessionSnapshot {
        snapshot: LoadedSession,
    },
    CloseRuntime,
    SubagentRegistryChanged,
    ResolveToolPause {
        tool_use_id: String,
        response: ToolPauseResponse,
    },
    ResolvePlanApproval {
        plan_id: String,
        action: PlanApprovalAction,
    },
}

/// runtime 发往 server/facade 的事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeToServerEvent {
    RunStarted,
    UserMessageInjected {
        item: HistoryItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_echo_id: Option<String>,
    },
    RunFinished,
    Notification(Notification),
    ModelChanged {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
        context_window: Option<u32>,
    },
    UsageChanged(SessionUsageSnapshot),
    UsageTotalsChanged {
        total_tokens: i64,
        total_cached_tokens: i64,
    },
    SessionSnapshot {
        session_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<SubagentSnapshot>,
        usage: SessionUsageSnapshot,
    },
    SessionTitleChanged {
        title: Option<String>,
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
    SubagentStarted(SubagentStartedEvent),
    SubagentMessageProduced(SubagentMessageEvent),
    SubagentToolUse(SubagentToolUseEvent),
    SubagentToolResult(SubagentToolResultEvent),
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
    use omini_domain::events::ActiveProfile;
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
    use crate::RuntimeToServerEvent;
    use omini_domain::display::{DisplayMessage, HistoryItem};
    use omini_domain::events::ActiveProfile;
    use omini_domain::message::{ContentBlock, Message, Role};
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
