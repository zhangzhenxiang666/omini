use super::*;

pub(super) fn thinking_effort_to_protocol(
    effort: omini_core::types::config::ThinkingEffort,
) -> protocol::ThinkingEffort {
    match effort {
        omini_core::types::config::ThinkingEffort::None => protocol::ThinkingEffort::None,
        omini_core::types::config::ThinkingEffort::Low => protocol::ThinkingEffort::Low,
        omini_core::types::config::ThinkingEffort::Medium => protocol::ThinkingEffort::Medium,
        omini_core::types::config::ThinkingEffort::High => protocol::ThinkingEffort::High,
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

/// 将持久化 snapshot 转成一组 legacy runtime 事件供 TUI 恢复 UI。
pub(super) fn session_snapshot_events(
    snapshot: omini_core::types::events::LoadedSession,
    context_window: Option<u32>,
    active_profile: ActiveProfile,
) -> Result<Vec<RuntimeEvent>, CoreError> {
    let mut usage = snapshot.usage;
    usage.context_window = context_window;
    let events = [
        omini_core::types::events::RuntimeToUiEvent::SessionTitleChanged {
            title: snapshot.title,
        },
        omini_core::types::events::RuntimeToUiEvent::ModelChanged {
            provider: snapshot.provider,
            model: snapshot.model,
            thinking_effort: snapshot.thinking_effort,
            context_window,
        },
        omini_core::types::events::RuntimeToUiEvent::ActiveProfileChanged(active_profile),
        omini_core::types::events::RuntimeToUiEvent::SessionChanged {
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

/// 把 core/TUI 内部事件编码成协议 `RuntimeEvent`。
pub(super) fn runtime_event_from_internal(
    event: omini_core::types::events::RuntimeToUiEvent,
) -> Result<RuntimeEvent, CoreError> {
    let payload = serde_json::to_value(event)
        .map_err(|error| CoreError::new(format!("Failed to encode runtime event: {error}")))?;
    Ok(RuntimeEvent::new(runtime_event_kind(&payload), payload))
}

/// 从序列化后的 runtime payload 中取出旧协议事件名。
fn runtime_event_kind(payload: &serde_json::Value) -> String {
    payload
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("runtime.event")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
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
                "session_changed"
            ]
        );
        assert_eq!(events[0].payload["title"], "hello");
        assert_eq!(events[1].payload["context_window"], 1000);
        assert_eq!(events[2].payload["profile"], "plan");
        assert_eq!(events[3].payload["session_id"], "s1");
        assert_eq!(events[3].payload["usage"]["context_window"], 1000);
        assert_eq!(events[3].payload["messages"].as_array().unwrap().len(), 1);
    }
}
