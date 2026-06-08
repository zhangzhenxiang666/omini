use super::*;
use omini_core::types::events as event_types;

pub(super) fn thinking_effort_to_protocol(
    effort: omini_core::types::config::ThinkingEffort,
) -> protocol::ThinkingEffort {
    match effort {
        omini_core::types::config::ThinkingEffort::None => protocol::ThinkingEffort::None,
        omini_core::types::config::ThinkingEffort::Low => protocol::ThinkingEffort::Low,
        omini_core::types::config::ThinkingEffort::Medium => protocol::ThinkingEffort::Medium,
        omini_core::types::config::ThinkingEffort::High => protocol::ThinkingEffort::High,
        omini_core::types::config::ThinkingEffort::XHigh => protocol::ThinkingEffort::XHigh,
        omini_core::types::config::ThinkingEffort::Max => protocol::ThinkingEffort::Max,
    }
}

pub(super) fn thinking_effort_from_protocol(
    effort: protocol::ThinkingEffort,
) -> omini_core::types::config::ThinkingEffort {
    match effort {
        protocol::ThinkingEffort::None => omini_core::types::config::ThinkingEffort::None,
        protocol::ThinkingEffort::Low => omini_core::types::config::ThinkingEffort::Low,
        protocol::ThinkingEffort::Medium => omini_core::types::config::ThinkingEffort::Medium,
        protocol::ThinkingEffort::High => omini_core::types::config::ThinkingEffort::High,
        protocol::ThinkingEffort::XHigh => omini_core::types::config::ThinkingEffort::XHigh,
        protocol::ThinkingEffort::Max => omini_core::types::config::ThinkingEffort::Max,
    }
}

pub(super) fn active_profile_from_protocol(profile: protocol::ActiveProfile) -> ActiveProfile {
    match profile {
        protocol::ActiveProfile::Main => ActiveProfile::Main,
        protocol::ActiveProfile::Auto => ActiveProfile::Auto,
        protocol::ActiveProfile::Plan => ActiveProfile::Plan,
    }
}

pub(super) fn models_response_from_settings(
    settings: &omini_core::types::config::Settings,
) -> protocol::ModelsResponse {
    let mut providers = settings
        .providers
        .iter()
        .map(|(id, provider)| protocol::ProviderInfo {
            id: id.clone(),
            name: provider.name.clone(),
            endpoint: provider.endpoint,
            base_url: provider.base_url.clone(),
            models: provider
                .models
                .iter()
                .map(|model| protocol::ModelInfo {
                    id: model.id.clone(),
                    name: model.name.clone(),
                    limit: model.limit,
                    thinking: model.thinking,
                    input_modalities: model.input_modalities.clone(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    protocol::ModelsResponse {
        providers,
        current_provider: settings.active_provider.clone(),
        current_model: settings.model.clone(),
    }
}

/// 将数据库会话记录压缩成协议层会话摘要。
pub(super) fn session_summary_from_store(session: Session) -> protocol::SessionSummary {
    protocol::SessionSummary {
        id: session.id,
        title: session.title.unwrap_or_default(),
        model: session.model,
        provider: session.provider,
        created_at: session.created_at,
        updated_at: session.updated_at,
        runtime_state: None,
    }
}

/// 从首条用户输入生成默认会话标题。
pub(super) fn initial_session_title_from_input(input: &protocol::UserInput) -> Option<String> {
    let title = input.text.trim();
    (!title.is_empty()).then(|| title.chars().take(300).collect())
}

/// 将持久化 snapshot 转成一组 runtime 事件供 TUI 恢复 UI。
pub(super) fn session_snapshot_events(
    snapshot: omini_core::types::events::LoadedSession,
    context_window: Option<u32>,
    active_profile: ActiveProfile,
) -> Result<Vec<RuntimeEvent>, CoreError> {
    let mut usage = snapshot.usage;
    usage.context_window = context_window;
    let events = [
        RuntimeToServerEvent::SessionTitleChanged {
            title: snapshot.title,
        },
        RuntimeToServerEvent::ModelChanged {
            provider: snapshot.provider,
            model: snapshot.model,
            thinking_effort: snapshot.thinking_effort,
            context_window,
        },
        RuntimeToServerEvent::ActiveProfileChanged(active_profile),
        RuntimeToServerEvent::SessionSnapshot {
            session_id: Some(snapshot.session_id),
            messages: snapshot.messages,
            subagents: snapshot.subagents,
            usage,
        },
    ];

    events
        .into_iter()
        .map(runtime_event_from_internal)
        .collect()
}

/// 把 core 内部 runtime 事件编码成协议 `RuntimeEvent`。
pub(super) fn runtime_event_from_internal(
    event: RuntimeToServerEvent,
) -> Result<RuntimeEvent, CoreError> {
    Ok(RuntimeEvent::new(typed_runtime_event_from_internal(event)))
}

pub(super) fn thinking_display_changed_event(show: bool) -> RuntimeEvent {
    RuntimeEvent::new(protocol::TypedRuntimeEvent::ThinkingDisplayChanged(
        protocol::ThinkingDisplayChangedEvent { show },
    ))
}

fn typed_runtime_event_from_internal(event: RuntimeToServerEvent) -> protocol::TypedRuntimeEvent {
    match event {
        RuntimeToServerEvent::RunStarted => protocol::TypedRuntimeEvent::RunStarted,
        RuntimeToServerEvent::UserMessageInjected {
            item,
            client_echo_id,
        } => protocol::TypedRuntimeEvent::UserMessageInjected {
            item,
            client_echo_id,
        },
        RuntimeToServerEvent::RunFinished => protocol::TypedRuntimeEvent::RunFinished,
        RuntimeToServerEvent::Notification(notification) => {
            protocol::TypedRuntimeEvent::Notification(protocol::NotificationEvent {
                level: notification_level_to_protocol(notification.kind),
                message: notification.message,
                details: notification.details,
            })
        }
        RuntimeToServerEvent::ModelChanged {
            provider,
            model,
            thinking_effort,
            context_window,
        } => protocol::TypedRuntimeEvent::ModelChanged(protocol::ModelChangedEvent {
            provider,
            model,
            thinking_effort,
            context_window,
        }),
        RuntimeToServerEvent::UsageChanged(usage) => {
            protocol::TypedRuntimeEvent::UsageChanged(usage)
        }
        RuntimeToServerEvent::UsageTotalsChanged {
            total_tokens,
            total_cached_tokens,
        } => protocol::TypedRuntimeEvent::UsageTotalsChanged(protocol::UsageTotalsChangedEvent {
            total_tokens,
            total_cached_tokens,
        }),
        RuntimeToServerEvent::SessionSnapshot {
            session_id,
            messages,
            subagents,
            usage,
        } => protocol::TypedRuntimeEvent::SessionSnapshot(protocol::SessionSnapshotEvent {
            session_id,
            messages,
            subagents,
            usage,
        }),
        RuntimeToServerEvent::SessionTitleChanged { title } => {
            protocol::TypedRuntimeEvent::SessionTitleChanged(protocol::SessionTitleChangedEvent {
                title,
            })
        }
        RuntimeToServerEvent::ActiveProfileChanged(profile) => {
            protocol::TypedRuntimeEvent::ActiveProfileChanged(protocol::ActiveProfileChangedEvent {
                profile,
            })
        }
        RuntimeToServerEvent::AgentManagementUpdated { records } => {
            protocol::TypedRuntimeEvent::AgentManagementUpdated { records }
        }
        RuntimeToServerEvent::TurnStarted => protocol::TypedRuntimeEvent::TurnStarted,
        RuntimeToServerEvent::TurnEnded => protocol::TypedRuntimeEvent::TurnEnded,
        RuntimeToServerEvent::ThinkingDelta(delta) => {
            protocol::TypedRuntimeEvent::ThinkingDelta(protocol::RuntimeDeltaEvent { delta })
        }
        RuntimeToServerEvent::TextDelta(delta) => {
            protocol::TypedRuntimeEvent::TextDelta(protocol::RuntimeDeltaEvent { delta })
        }
        RuntimeToServerEvent::ProposedPlanDelta(delta) => {
            protocol::TypedRuntimeEvent::ProposedPlanDelta(protocol::RuntimeDeltaEvent { delta })
        }
        RuntimeToServerEvent::ToolUse(tool_use) => protocol::TypedRuntimeEvent::ToolUse(tool_use),
        RuntimeToServerEvent::ToolResult(tool_result) => {
            protocol::TypedRuntimeEvent::ToolResult(tool_result)
        }
        RuntimeToServerEvent::ToolPauseRequested(request) => {
            protocol::TypedRuntimeEvent::ToolPauseRequested(request)
        }
        RuntimeToServerEvent::PlanSubmitted(plan) => {
            protocol::TypedRuntimeEvent::PlanSubmitted(plan)
        }
        RuntimeToServerEvent::PlanApprovalResolved { plan_id, action } => {
            protocol::TypedRuntimeEvent::PlanApprovalResolved(protocol::PlanApprovalResolvedEvent {
                plan_id,
                action,
            })
        }
        RuntimeToServerEvent::CompactSummaryStarted(event) => {
            protocol::TypedRuntimeEvent::CompactSummaryStarted(
                protocol::CompactSummaryStartedEvent {
                    trigger: event.trigger,
                    session_id: event.session_id,
                    agent_label: event.agent_label,
                },
            )
        }
        RuntimeToServerEvent::CompactSummaryDelta(event) => {
            protocol::TypedRuntimeEvent::CompactSummaryDelta(protocol::CompactSummaryDeltaEvent {
                trigger: event.trigger,
                delta: event.delta,
                session_id: event.session_id,
                agent_label: event.agent_label,
            })
        }
        RuntimeToServerEvent::CompactSummaryFinished(event) => {
            protocol::TypedRuntimeEvent::CompactSummaryFinished(
                protocol::CompactSummaryFinishedEvent {
                    trigger: event.trigger,
                    summary: event.summary,
                    after_tokens: event.after_tokens,
                    session_id: event.session_id,
                    agent_label: event.agent_label,
                },
            )
        }
        RuntimeToServerEvent::CompactSummaryFailed(event) => {
            protocol::TypedRuntimeEvent::CompactSummaryFailed(protocol::CompactSummaryFailedEvent {
                trigger: event.trigger,
                message: event.message,
                session_id: event.session_id,
                agent_label: event.agent_label,
            })
        }
        RuntimeToServerEvent::SubagentStarted(event) => {
            protocol::TypedRuntimeEvent::SubagentStarted(event)
        }
        RuntimeToServerEvent::SubagentMessageProduced(event) => {
            protocol::TypedRuntimeEvent::SubagentMessageProduced(event)
        }
        RuntimeToServerEvent::SubagentToolUse(event) => {
            protocol::TypedRuntimeEvent::SubagentToolUse(event)
        }
        RuntimeToServerEvent::SubagentToolResult(event) => {
            protocol::TypedRuntimeEvent::SubagentToolResult(event)
        }
        RuntimeToServerEvent::SubagentFinished(event) => {
            protocol::TypedRuntimeEvent::SubagentFinished(event)
        }
    }
}

fn tool_pause_kind_to_protocol(kind: &event_types::ToolPauseKind) -> protocol::ToolPauseEventKind {
    match kind {
        event_types::ToolPauseKind::Permission(_) => protocol::ToolPauseEventKind::Permission,
        event_types::ToolPauseKind::UserInput(_) => protocol::ToolPauseEventKind::UserInput,
    }
}

pub(super) fn tool_pause_request_kind(
    request: &protocol::ToolPauseRequestedEvent,
) -> protocol::ToolPauseEventKind {
    tool_pause_kind_to_protocol(&request.kind)
}

fn notification_level_to_protocol(
    kind: event_types::NotificationKind,
) -> protocol::NotificationLevel {
    match kind {
        event_types::NotificationKind::Info => protocol::NotificationLevel::Info,
        event_types::NotificationKind::Warn => protocol::NotificationLevel::Warn,
        event_types::NotificationKind::Error => protocol::NotificationLevel::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_core::types::events as event_types;
    use omini_core::types::events::SessionUsageSnapshot;

    #[test]
    fn initial_session_title_from_input_trims_and_limits_text() {
        let input = protocol::UserInput::plain(format!("  {}  ", "a".repeat(400)));

        let title = initial_session_title_from_input(&input).expect("title should be present");

        assert_eq!(title.len(), 300);
        assert!(title.chars().all(|ch| ch == 'a'));
    }

    #[test]
    fn initial_session_title_from_input_skips_blank_text() {
        let input = protocol::UserInput::plain("   ");

        assert_eq!(initial_session_title_from_input(&input), None);
    }

    #[test]
    fn session_snapshot_events_replay_current_session_state() {
        let events = session_snapshot_events(
            LoadedSession {
                session_id: "s1".to_string(),
                provider: "main".to_string(),
                model: "test-model".to_string(),
                thinking_effort: None,
                active_profile: ActiveProfile::Main,
                title: Some("hello".to_string()),
                messages: vec![HistoryItem::Message(Message::from_user_text(
                    "hello".to_string(),
                ))],
                subagents: Vec::new(),
                usage: SessionUsageSnapshot {
                    current_context_tokens: 3,
                    total_tokens: 5,
                    total_cached_tokens: 1,
                    context_window: None,
                },
            },
            Some(1000),
            ActiveProfile::Plan,
        )
        .expect("snapshot events should encode");

        assert_eq!(
            events.iter().map(|event| event.kind()).collect::<Vec<_>>(),
            vec![
                "session_title_changed",
                "model_changed",
                "active_profile_changed",
                "session_snapshot"
            ]
        );
        assert!(matches!(
            &events[0].event,
            protocol::TypedRuntimeEvent::SessionTitleChanged(event)
                if event.title.as_deref() == Some("hello")
        ));
        assert!(matches!(
            &events[1].event,
            protocol::TypedRuntimeEvent::ModelChanged(event)
                if event.context_window == Some(1000)
        ));
        assert!(matches!(
            &events[2].event,
            protocol::TypedRuntimeEvent::ActiveProfileChanged(event)
                if event.profile == protocol::ActiveProfile::Plan
        ));
        assert!(matches!(
            &events[3].event,
            protocol::TypedRuntimeEvent::SessionSnapshot(event)
                if event.session_id.as_deref() == Some("s1")
                    && event.usage.context_window == Some(1000)
                    && event.messages.len() == 1
        ));
    }

    #[test]
    fn active_profile_changed_is_typed() {
        let event = runtime_event_from_internal(RuntimeToServerEvent::ActiveProfileChanged(
            ActiveProfile::Plan,
        ))
        .expect("event should encode");

        assert_eq!(event.kind(), "active_profile_changed");
        assert!(matches!(
            event.event,
            protocol::TypedRuntimeEvent::ActiveProfileChanged(
                protocol::ActiveProfileChangedEvent {
                    profile: protocol::ActiveProfile::Plan,
                },
            )
        ));
    }

    #[test]
    fn compact_summary_finished_is_typed() {
        let event = runtime_event_from_internal(RuntimeToServerEvent::CompactSummaryFinished(
            event_types::CompactSummaryFinishedEvent {
                trigger: event_types::CompactTrigger::Manual,
                summary: "summary".to_string(),
                after_tokens: 42,
                session_id: Some("session_1".to_string()),
                agent_label: None,
            },
        ))
        .expect("event should encode");

        assert_eq!(event.kind(), "compact_summary_finished");
        assert_eq!(
            event.event,
            protocol::TypedRuntimeEvent::CompactSummaryFinished(
                protocol::CompactSummaryFinishedEvent {
                    trigger: protocol::CompactTrigger::Manual,
                    summary: "summary".to_string(),
                    after_tokens: 42,
                    session_id: Some("session_1".to_string()),
                    agent_label: None,
                },
            )
        );
    }

    #[test]
    fn plan_approval_resolved_is_typed() {
        let event = runtime_event_from_internal(RuntimeToServerEvent::PlanApprovalResolved {
            plan_id: "plan_1".to_string(),
            action: event_types::PlanApprovalAction::ContinueDiscussing,
        })
        .expect("event should encode");

        assert_eq!(event.kind(), "plan_approval_resolved");
        assert_eq!(
            event.event,
            protocol::TypedRuntimeEvent::PlanApprovalResolved(
                protocol::PlanApprovalResolvedEvent {
                    plan_id: "plan_1".to_string(),
                    action: protocol::PlanApprovalAction::ContinueDiscussing,
                },
            )
        );
    }

    #[test]
    fn thinking_display_changed_is_typed() {
        let event = thinking_display_changed_event(false);

        assert_eq!(event.kind(), "thinking_display_changed");
        assert!(matches!(
            event.event,
            protocol::TypedRuntimeEvent::ThinkingDisplayChanged(
                protocol::ThinkingDisplayChangedEvent { show: false }
            )
        ));
    }
}
