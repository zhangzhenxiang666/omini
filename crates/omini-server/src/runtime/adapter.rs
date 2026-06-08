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
    let key_event = key_runtime_event_from_internal(&event);
    let payload = serde_json::to_value(event).map_err(|source| CoreError::RuntimeEventEncode {
        source: Box::new(source),
    })?;
    let mut event = RuntimeEvent::new(runtime_event_kind(&payload), payload);
    if let Some(key_event) = key_event {
        event = event.with_key_event(key_event);
    }
    Ok(event)
}

pub(super) fn thinking_display_changed_event(show: bool) -> RuntimeEvent {
    RuntimeEvent::new(
        "thinking_display_changed",
        serde_json::json!({
            "type": "thinking_display_changed",
            "show": show,
        }),
    )
}

/// 从序列化后的 runtime payload 中取出旧协议事件名。
fn runtime_event_kind(payload: &serde_json::Value) -> String {
    payload
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("runtime.event")
        .to_string()
}

fn key_runtime_event_from_internal(
    event: &RuntimeToServerEvent,
) -> Option<protocol::KeyRuntimeEvent> {
    match event {
        RuntimeToServerEvent::RunStarted => Some(protocol::KeyRuntimeEvent::RunStarted),
        RuntimeToServerEvent::RunFinished => Some(protocol::KeyRuntimeEvent::RunFinished),
        RuntimeToServerEvent::Notification(notification) => Some(
            protocol::KeyRuntimeEvent::Notification(protocol::NotificationEvent {
                level: notification_level_to_protocol(notification.kind),
                message: notification.message.clone(),
                details: notification.details.clone(),
            }),
        ),
        RuntimeToServerEvent::ActiveProfileChanged(profile) => {
            Some(protocol::KeyRuntimeEvent::ActiveProfileChanged(
                protocol::ActiveProfileChangedEvent { profile: *profile },
            ))
        }
        RuntimeToServerEvent::ToolPauseRequested(request) => Some(
            protocol::KeyRuntimeEvent::ToolPauseRequested(protocol::ToolPauseRequestedEvent {
                tool_use_id: request.tool_use_id.clone(),
                tool_name: request.tool_name.clone(),
                kind: match &request.kind {
                    event_types::ToolPauseKind::Permission(_) => {
                        protocol::ToolPauseEventKind::Permission
                    }
                    event_types::ToolPauseKind::UserInput(_) => {
                        protocol::ToolPauseEventKind::UserInput
                    }
                },
                source_session_id: request.source_session_id.clone(),
                source_agent_label: request.source_agent_label.clone(),
            }),
        ),
        RuntimeToServerEvent::PlanSubmitted(plan) => Some(
            protocol::KeyRuntimeEvent::PlanSubmitted(protocol::PlanSubmittedEvent {
                plan_id: plan.id.clone(),
                title: plan.title.clone(),
                markdown: plan.markdown.clone(),
            }),
        ),
        RuntimeToServerEvent::PlanApprovalResolved { plan_id, action } => Some(
            protocol::KeyRuntimeEvent::PlanApprovalResolved(protocol::PlanApprovalResolvedEvent {
                plan_id: plan_id.clone(),
                action: *action,
            }),
        ),
        RuntimeToServerEvent::CompactSummaryStarted(event) => {
            Some(protocol::KeyRuntimeEvent::CompactSummaryStarted(
                protocol::CompactSummaryStartedEvent {
                    trigger: event.trigger,
                    session_id: event.session_id.clone(),
                    agent_label: event.agent_label.clone(),
                },
            ))
        }
        RuntimeToServerEvent::CompactSummaryDelta(event) => Some(
            protocol::KeyRuntimeEvent::CompactSummaryDelta(protocol::CompactSummaryDeltaEvent {
                trigger: event.trigger,
                delta: event.delta.clone(),
                session_id: event.session_id.clone(),
                agent_label: event.agent_label.clone(),
            }),
        ),
        RuntimeToServerEvent::CompactSummaryFinished(event) => {
            Some(protocol::KeyRuntimeEvent::CompactSummaryFinished(
                protocol::CompactSummaryFinishedEvent {
                    trigger: event.trigger,
                    summary: event.summary.clone(),
                    after_tokens: event.after_tokens,
                    session_id: event.session_id.clone(),
                    agent_label: event.agent_label.clone(),
                },
            ))
        }
        RuntimeToServerEvent::CompactSummaryFailed(event) => Some(
            protocol::KeyRuntimeEvent::CompactSummaryFailed(protocol::CompactSummaryFailedEvent {
                trigger: event.trigger,
                message: event.message.clone(),
                session_id: event.session_id.clone(),
                agent_label: event.agent_label.clone(),
            }),
        ),
        RuntimeToServerEvent::SessionSnapshot {
            session_id,
            messages,
            subagents,
            usage,
        } => Some(protocol::KeyRuntimeEvent::SessionSnapshot(
            protocol::SessionSnapshotEvent {
                session_id: session_id.clone(),
                message_count: messages.len(),
                subagent_count: subagents.len(),
                usage: protocol::SessionUsage {
                    current_context_tokens: usage.current_context_tokens,
                    total_tokens: usage.total_tokens,
                    total_cached_tokens: usage.total_cached_tokens,
                    context_window: usage.context_window,
                },
            },
        )),
        _ => None,
    }
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
            events
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "session_title_changed",
                "model_changed",
                "active_profile_changed",
                "session_snapshot"
            ]
        );
        assert_eq!(events[0].payload["title"], "hello");
        assert_eq!(events[1].payload["context_window"], 1000);
        assert_eq!(events[2].payload["profile"], "plan");
        assert_eq!(events[3].payload["session_id"], "s1");
        assert_eq!(events[3].payload["usage"]["context_window"], 1000);
        assert_eq!(events[3].payload["messages"].as_array().unwrap().len(), 1);
        assert_eq!(
            events[2].event,
            Some(protocol::KeyRuntimeEvent::ActiveProfileChanged(
                protocol::ActiveProfileChangedEvent {
                    profile: protocol::ActiveProfile::Plan,
                },
            ))
        );
        assert_eq!(
            events[3].event,
            Some(protocol::KeyRuntimeEvent::SessionSnapshot(
                protocol::SessionSnapshotEvent {
                    session_id: Some("s1".to_string()),
                    message_count: 1,
                    subagent_count: 0,
                    usage: protocol::SessionUsage {
                        current_context_tokens: 3,
                        total_tokens: 5,
                        total_cached_tokens: 1,
                        context_window: Some(1000),
                    },
                },
            ))
        );
    }

    #[test]
    fn active_profile_changed_has_typed_overlay() {
        let event = runtime_event_from_internal(RuntimeToServerEvent::ActiveProfileChanged(
            ActiveProfile::Plan,
        ))
        .expect("event should encode");

        assert_eq!(event.kind, "active_profile_changed");
        assert_eq!(event.payload["profile"], "plan");
        assert_eq!(
            event.event,
            Some(protocol::KeyRuntimeEvent::ActiveProfileChanged(
                protocol::ActiveProfileChangedEvent {
                    profile: protocol::ActiveProfile::Plan,
                },
            ))
        );
    }

    #[test]
    fn compact_summary_finished_has_typed_overlay() {
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

        assert_eq!(event.kind, "compact_summary_finished");
        assert_eq!(
            event.event,
            Some(protocol::KeyRuntimeEvent::CompactSummaryFinished(
                protocol::CompactSummaryFinishedEvent {
                    trigger: protocol::CompactTrigger::Manual,
                    summary: "summary".to_string(),
                    after_tokens: 42,
                    session_id: Some("session_1".to_string()),
                    agent_label: None,
                },
            ))
        );
    }

    #[test]
    fn plan_approval_resolved_has_typed_overlay() {
        let event = runtime_event_from_internal(RuntimeToServerEvent::PlanApprovalResolved {
            plan_id: "plan_1".to_string(),
            action: event_types::PlanApprovalAction::ContinueDiscussing,
        })
        .expect("event should encode");

        assert_eq!(event.kind, "plan_approval_resolved");
        assert_eq!(
            event.event,
            Some(protocol::KeyRuntimeEvent::PlanApprovalResolved(
                protocol::PlanApprovalResolvedEvent {
                    plan_id: "plan_1".to_string(),
                    action: protocol::PlanApprovalAction::ContinueDiscussing,
                },
            ))
        );
    }

    #[test]
    fn thinking_display_changed_preserves_legacy_payload() {
        let event = thinking_display_changed_event(false);

        assert_eq!(event.kind, "thinking_display_changed");
        assert_eq!(event.payload["type"], "thinking_display_changed");
        assert_eq!(event.payload["show"], false);
        assert_eq!(event.event, None);
    }
}
