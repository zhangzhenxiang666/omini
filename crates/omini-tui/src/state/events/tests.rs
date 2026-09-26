use super::*;
use crate::display::{DisplayMention, DisplayMessage, MentionKind};
use crate::types::config::{ModelConfig, ProviderProfile, ProviderType};
use crate::types::events::{
    CompactEvent, CompactSummaryDeltaEvent, CompactSummaryFailedEvent, CompactSummaryFinishedEvent,
    CompactTrigger, ThreadSummary,
};
use chrono::{Duration, Utc};
use omini_domain::conversation::UserInput;
use omini_domain::input::{InputPart, UserInputIntent};
use std::collections::HashMap;

fn model_selection_request() -> InteractionRequest {
    InteractionRequest::ModelSelection {
        providers: HashMap::from([(
            "openai".to_string(),
            ProviderProfile {
                name: "OpenAI".to_string(),
                endpoint: ProviderType::OpenAI,
                base_url: "https://openai.example".to_string(),
                models: vec![ModelConfig {
                    id: "reasoner".to_string(),
                    name: None,
                    limit: 1000,
                    thinking: true,
                    input_modalities: None,
                    extra_body: None,
                    extra_headers: None,
                }],
            },
        )]),
        current_provider: "openai".to_string(),
        current_model: "reasoner".to_string(),
    }
}

fn thread_summary(id: &str, updated_at: chrono::DateTime<Utc>) -> ThreadSummary {
    ThreadSummary {
        id: id.to_string(),
        title: id.to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        created_at: updated_at,
        updated_at,
        runtime_state: None,
    }
}

fn subagent_display_message(description: &str) -> DisplayMessage {
    DisplayMessage {
        role: Role::User,
        text: "@code-reviewer review this".to_string(),
        mentions: vec![DisplayMention {
            start_char: 0,
            end_char: 14,
            kind: MentionKind::Subagent,
            label: "code-reviewer".to_string(),
            target: "code-reviewer".to_string(),
            description: description.to_string(),
        }],
    }
}

fn history_item_for_subagent(label: &str) -> HistoryItem {
    HistoryItem::UserInput(UserInput {
        intent: UserInputIntent::Message,
        parts: vec![
            InputPart::Subagent {
                name: "code-reviewer".to_string(),
                label: Some(label.to_string()),
            },
            InputPart::Text {
                text: " review this".to_string(),
            },
        ],
        attachments: Vec::new(),
    })
}

#[test]
fn model_selection_defaults_missing_thinking_effort_to_medium() {
    let mut state = UiState::new();
    state.status_bar.thinking_effort = None;

    state.open_interaction_request(&model_selection_request());

    let Some(InteractionStep::ModelSelection { thinking_idx, .. }) = state.interaction_step else {
        panic!("expected model selection interaction");
    };
    assert_eq!(thinking_idx, 2);
}

#[test]
fn model_selection_preserves_explicit_no_thinking_effort() {
    let mut state = UiState::new();
    state.status_bar.thinking_effort = Some(ThinkingEffort::None);

    state.open_interaction_request(&model_selection_request());

    let Some(InteractionStep::ModelSelection { thinking_idx, .. }) = state.interaction_step else {
        panic!("expected model selection interaction");
    };
    assert_eq!(thinking_idx, 0);
}

#[test]
fn model_selection_preserves_max_thinking_effort() {
    let mut state = UiState::new();
    state.status_bar.thinking_effort = Some(ThinkingEffort::Max);

    state.open_interaction_request(&model_selection_request());

    let Some(InteractionStep::ModelSelection { thinking_idx, .. }) = state.interaction_step else {
        panic!("expected model selection interaction");
    };
    assert_eq!(thinking_idx, 5);
}

#[test]
fn thread_selection_sorts_by_updated_at_descending() {
    let now = Utc::now();
    let mut state = UiState::new();
    let request = InteractionRequest::ThreadSelection {
        threads: vec![
            thread_summary("middle", now - Duration::minutes(1)),
            thread_summary("oldest", now - Duration::minutes(2)),
            thread_summary("newest", now),
        ],
    };

    state.open_interaction_request(&request);

    let Some(InteractionStep::Thread {
        threads,
        all_threads,
        ..
    }) = state.interaction_step
    else {
        panic!("expected thread selection interaction");
    };
    let ids = threads
        .iter()
        .map(|thread| thread.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["newest", "middle", "oldest"]);
    assert_eq!(threads, all_threads);
}

#[test]
fn usage_totals_changed_preserves_current_context_usage() {
    let mut state = UiState::new();
    state.status_bar.current_context_tokens = 123;
    state.status_bar.context_window = Some(456);

    state.apply_event(RuntimeToUiEvent::UsageTotalsChanged {
        total_tokens: 789,
        total_cached_tokens: 12,
    });

    assert_eq!(state.status_bar.current_context_tokens, 123);
    assert_eq!(state.status_bar.context_window, Some(456));
    assert_eq!(state.status_bar.total_tokens, 789);
    assert_eq!(state.status_bar.total_cached_tokens, 12);
}

#[test]
fn user_message_injected_does_not_duplicate_optimistic_echo() {
    let mut state = UiState::new();
    state.messages.push(UiMessage::Display(DisplayMessage {
        role: Role::User,
        text: "hello".to_string(),
        mentions: Vec::new(),
    }));

    state.apply_event(RuntimeToUiEvent::UserMessageInjected {
        item: HistoryItem::UserInput(UserInput {
            intent: UserInputIntent::Message,
            parts: vec![InputPart::Text {
                text: "hello".to_string(),
            }],
            attachments: Vec::new(),
        }),
        client_echo_id: None,
    });

    assert_eq!(state.messages.len(), 1);
}

#[test]
fn user_message_injected_uses_client_echo_id_for_display_metadata_differences() {
    let mut state = UiState::new();
    let local = subagent_display_message("Review code changes");

    state.push_optimistic_echo(UiMessage::Display(local.clone()), "echo-1".to_string());
    state.apply_event(RuntimeToUiEvent::UserMessageInjected {
        item: history_item_for_subagent("subagent"),
        client_echo_id: Some("echo-1".to_string()),
    });

    assert_eq!(state.messages, vec![UiMessage::Display(local)]);
    assert!(state.pending_client_echoes.is_empty());
}

#[test]
fn user_message_injected_without_client_echo_id_appends_different_message() {
    let mut state = UiState::new();
    let local = subagent_display_message("Review code changes");

    state.messages.push(UiMessage::Display(local));
    state.apply_event(RuntimeToUiEvent::UserMessageInjected {
        item: history_item_for_subagent("subagent"),
        client_echo_id: None,
    });

    assert_eq!(state.messages.len(), 2);
}

#[test]
fn user_message_injected_with_unmatched_client_echo_id_appends_for_observers() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::UserMessageInjected {
        item: history_item_for_subagent("subagent"),
        client_echo_id: Some("echo-1".to_string()),
    });

    assert_eq!(
        state.messages,
        vec![UiMessage::Display(crate::display::user_input_message(
            &UserInput {
                intent: UserInputIntent::Message,
                parts: vec![
                    InputPart::Subagent {
                        name: "code-reviewer".to_string(),
                        label: Some("subagent".to_string()),
                    },
                    InputPart::Text {
                        text: " review this".to_string(),
                    },
                ],
                attachments: Vec::new(),
            }
        ))]
    );
}

#[test]
fn thinking_delta_starts_timer_and_text_delta_settles_duration() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::RunStarted);
    assert_eq!(state.thinking_started_at, None);

    state.apply_event(RuntimeToUiEvent::ThinkingDelta("分析".to_string()));
    assert!(state.thinking_started_at.is_some());

    // 首个非思考内容到达时结算：计时关闭，时长写入未计时的 Thinking 块
    state.apply_event(RuntimeToUiEvent::TextDelta("结论".to_string()));
    assert_eq!(state.thinking_started_at, None);
    let pending = state.pending_assistant.as_ref().expect("pending exists");
    assert!(matches!(
        pending.content.first(),
        Some(ContentBlock::Thinking(tb)) if tb.duration_ms.is_some()
    ));
}

#[test]
fn settle_writes_only_the_latest_unmeasured_thinking_block() {
    let mut state = UiState::new();
    state.apply_event(RuntimeToUiEvent::RunStarted);
    state.apply_event(RuntimeToUiEvent::ThinkingDelta("第一段".to_string()));
    state.apply_event(RuntimeToUiEvent::TextDelta("正文".to_string()));

    // 第二段思考：只结算到最后一个未计时块，第一段的时长不被覆盖
    state.apply_event(RuntimeToUiEvent::ThinkingDelta("第二段".to_string()));
    let first_ms = match state.pending_assistant.as_ref().unwrap().content.first() {
        Some(ContentBlock::Thinking(tb)) => tb.duration_ms,
        _ => panic!("first block should be thinking"),
    };
    state.apply_event(RuntimeToUiEvent::TurnEnded);

    let blocks = &state.messages.last().unwrap().as_message().unwrap().content;
    let durations: Vec<Option<u64>> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Thinking(tb) => Some(tb.duration_ms),
            _ => None,
        })
        .collect();
    assert_eq!(durations.len(), 2);
    assert!(durations.iter().all(Option::is_some));
    assert_eq!(durations[0], first_ms);
}

#[test]
fn run_started_and_thread_snapshot_reset_thinking_timer() {
    let mut state = UiState::new();
    state.apply_event(RuntimeToUiEvent::RunStarted);
    state.apply_event(RuntimeToUiEvent::ThinkingDelta("思考".to_string()));
    assert!(state.thinking_started_at.is_some());

    state.apply_event(RuntimeToUiEvent::RunStarted);
    assert_eq!(state.thinking_started_at, None);
}

#[test]
fn compact_summary_delta_streams_into_single_ui_message() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::CompactSummaryDelta(
        CompactSummaryDeltaEvent {
            trigger: CompactTrigger::Manual,
            delta: "first ".to_string(),
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));
    state.apply_event(RuntimeToUiEvent::CompactSummaryDelta(
        CompactSummaryDeltaEvent {
            trigger: CompactTrigger::Manual,
            delta: "second".to_string(),
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));

    let Some(text) = &state.pending_compact_summary else {
        panic!("expected pending compact summary");
    };
    assert!(text.contains("first second"));
}

#[test]
fn compact_summary_started_creates_new_summary_message() {
    let mut state = UiState::new();
    state.messages.push(UiMessage::CompactSummary {
        text: "previous".to_string(),
    });

    state.apply_event(RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
        trigger: CompactTrigger::Manual,
        thread_id: Some("thread".to_string()),
        agent_label: None,
    }));
    state.apply_event(RuntimeToUiEvent::CompactSummaryDelta(
        CompactSummaryDeltaEvent {
            trigger: CompactTrigger::Manual,
            delta: "new".to_string(),
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));

    // 历史摘要保留在 messages 中，新的流式内容在 pending 中
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.pending_compact_summary.as_deref(), Some("new"));
}

#[test]
fn compact_summary_finished_replaces_streamed_text_and_updates_context_tokens() {
    let mut state = UiState::new();
    state.pending_compact_summary = Some("partial".to_string());

    state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
        CompactSummaryFinishedEvent {
            trigger: CompactTrigger::Manual,
            summary: "final summary".to_string(),
            after_tokens: 250,
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));

    assert_eq!(state.status_bar.current_context_tokens, 250);
    let Some(UiMessage::CompactSummary { text }) = state.messages.last() else {
        panic!("expected compact summary message");
    };
    assert_eq!(text, "final summary");
}

#[test]
fn manual_compact_summary_lifecycle_returns_status_to_idle() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
        trigger: CompactTrigger::Manual,
        thread_id: Some("thread".to_string()),
        agent_label: None,
    }));

    assert_eq!(state.agent_status, AgentStatus::Working);
    assert!(state.manual_compact_running);
    assert!(state.run_timer.is_some());

    state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
        CompactSummaryFinishedEvent {
            trigger: CompactTrigger::Manual,
            summary: "final summary".to_string(),
            after_tokens: 250,
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));

    assert_eq!(state.agent_status, AgentStatus::Idle);
    assert!(!state.manual_compact_running);
    assert!(state.run_timer.is_none());
}

#[test]
fn empty_compact_summary_finished_removes_loading_placeholder() {
    let mut state = UiState::new();
    // 模拟 Started 事件创建的流式占位
    state.pending_compact_summary = Some(String::new());

    state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
        CompactSummaryFinishedEvent {
            trigger: CompactTrigger::Manual,
            summary: String::new(),
            after_tokens: 250,
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));

    assert!(state.messages.is_empty());
    assert!(state.pending_compact_summary.is_none());
}

#[test]
fn auto_compact_summary_finished_does_not_force_idle() {
    let mut state = UiState::new();
    state.agent_status = AgentStatus::Working;
    state.pending_compact_summary = Some("partial".to_string());

    state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
        CompactSummaryFinishedEvent {
            trigger: CompactTrigger::Auto,
            summary: "final summary".to_string(),
            after_tokens: 250,
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));

    assert_eq!(state.agent_status, AgentStatus::Working);
}

#[test]
fn manual_compact_summary_failed_clears_empty_placeholder_and_status() {
    let mut state = UiState::new();
    state.apply_event(RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
        trigger: CompactTrigger::Manual,
        thread_id: Some("thread".to_string()),
        agent_label: None,
    }));

    state.apply_event(RuntimeToUiEvent::CompactSummaryFailed(
        CompactSummaryFailedEvent {
            trigger: CompactTrigger::Manual,
            message: "nope".to_string(),
            thread_id: Some("thread".to_string()),
            agent_label: None,
        },
    ));

    assert_eq!(state.agent_status, AgentStatus::Idle);
    assert!(!state.manual_compact_running);
    assert!(state.run_timer.is_none());
    assert!(matches!(
        state.messages.as_slice(),
        [UiMessage::Notification(notification)]
            if notification.kind == NotificationKind::Warn
    ));
}

#[test]
fn manual_compact_warning_returns_status_to_idle() {
    let mut state = UiState::new();
    state.begin_manual_compact();

    state.apply_event(RuntimeToUiEvent::warning(
        "没有可压缩的会话历史".to_string(),
    ));

    assert_eq!(state.agent_status, AgentStatus::Idle);
    assert!(!state.manual_compact_running);
    assert!(state.run_timer.is_none());
    assert!(matches!(
        state.messages.as_slice(),
        [UiMessage::Notification(notification)]
            if notification.kind == NotificationKind::Warn
    ));
}

#[test]
fn manual_compact_error_returns_status_to_idle() {
    let mut state = UiState::new();
    state.begin_manual_compact();

    state.apply_event(RuntimeToUiEvent::error(
        "This client is not connected to the session event stream".to_string(),
    ));

    assert_eq!(state.agent_status, AgentStatus::Idle);
    assert!(!state.manual_compact_running);
    assert!(state.run_timer.is_none());
    assert!(matches!(
        state.messages.as_slice(),
        [UiMessage::Notification(notification)]
            if notification.kind == NotificationKind::Error
    ));
}

#[test]
fn error_notification_does_not_fail_running_subagents() {
    use crate::state::SubagentNode;

    let mut state = UiState::new();
    state.subagents.insert(
        "sub-1".to_string(),
        SubagentNode {
            task_id: "task-1".to_string(),
            thread_id: "sub-1".to_string(),
            parent_thread_id: "main".to_string(),
            spawn_tool_use_id: "tool-1".to_string(),
            agent_label: "worker".to_string(),
            title: "Work".to_string(),
            execution_mode: AgentTaskExecutionMode::Background,
            status: TaskStatus::Running,
            messages: Vec::new(),
        },
    );

    state.apply_event(RuntimeToUiEvent::error(
        "Cannot handle this request while a run is active".to_string(),
    ));

    let sub = state.subagents.get("sub-1").unwrap();
    assert_eq!(sub.status, TaskStatus::Running);
}
