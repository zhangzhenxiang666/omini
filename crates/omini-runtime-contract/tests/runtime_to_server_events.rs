use chrono::{DateTime, TimeZone, Utc};
use omini_domain::display::{DisplayPlan, HistoryItem};
use omini_domain::events::{
    ActiveProfile, AgentTaskEvent, AgentTaskEventEnvelope, CompactEvent, CompactSummaryDeltaEvent,
    CompactSummaryFailedEvent, CompactSummaryFinishedEvent, CompactTrigger, Notification,
    NotificationKind, PlanApprovalAction, PlanExecutionProfile, ThreadUsageSnapshot, ToolPauseKind,
    ToolPauseRequest, UserInputPreview,
};
use omini_domain::message::{Message, Role, ToolResultBlock, ToolUseBlock};
use omini_runtime_contract::RuntimeToServerEvent;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn runtime_to_server_event_all_variants_keep_their_tagged_contract() {
    for (event, expected) in runtime_event_cases() {
        assert_eq!(runtime_event_type(&event), expected["type"]);
        assert_eq!(
            serde_json::to_value(&event).expect("runtime event should serialize"),
            expected
        );

        let decoded: RuntimeToServerEvent = serde_json::from_value(expected.clone())
            .expect("canonical runtime event should deserialize");
        assert_eq!(runtime_event_type(&decoded), expected["type"]);
        assert_eq!(
            serde_json::to_value(decoded).expect("decoded runtime event should serialize"),
            expected
        );
    }
}

#[test]
fn delta_and_profile_events_all_values_use_flat_named_payloads() {
    for (event, expected) in [
        (
            RuntimeToServerEvent::ThinkingDelta(String::new()),
            json!({"type": "thinking_delta", "delta": ""}),
        ),
        (
            RuntimeToServerEvent::TextDelta("回答\n第二行".into()),
            json!({"type": "text_delta", "delta": "回答\n第二行"}),
        ),
        (
            RuntimeToServerEvent::ProposedPlanDelta("\0计划".into()),
            json!({"type": "proposed_plan_delta", "delta": "\0计划"}),
        ),
    ] {
        assert_eq!(
            serde_json::to_value(&event).expect("delta event should serialize"),
            expected
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<RuntimeToServerEvent>(expected.clone())
                    .expect("delta event should deserialize")
            )
            .expect("delta event should reserialize"),
            expected
        );
    }

    for (profile, name) in [
        (ActiveProfile::Main, "main"),
        (ActiveProfile::Auto, "auto"),
        (ActiveProfile::Plan, "plan"),
    ] {
        let expected = json!({"type": "active_profile_changed", "profile": name});
        let event = RuntimeToServerEvent::ActiveProfileChanged(profile);
        assert_eq!(
            serde_json::to_value(event).expect("profile event should serialize"),
            expected
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<RuntimeToServerEvent>(expected.clone())
                    .expect("profile event should deserialize")
            )
            .expect("profile event should reserialize"),
            expected
        );
    }
}

#[test]
fn injected_message_missing_or_null_echo_id_defaults_to_none_and_is_omitted() {
    let canonical = json!({
        "type": "user_message_injected",
        "item": {"type": "message", "role": "user", "content": []}
    });

    for input in [
        canonical.clone(),
        json!({
            "type": "user_message_injected",
            "item": {"type": "message", "role": "user", "content": []},
            "client_echo_id": null
        }),
    ] {
        let event: RuntimeToServerEvent =
            serde_json::from_value(input).expect("absent echo id should deserialize");
        assert!(matches!(
            &event,
            RuntimeToServerEvent::UserMessageInjected {
                client_echo_id: None,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(event).expect("injected message should serialize"),
            canonical
        );
    }

    let with_echo = json!({
        "type": "user_message_injected",
        "item": {"type": "message", "role": "user", "content": []},
        "client_echo_id": "echo-终"
    });
    let event: RuntimeToServerEvent =
        serde_json::from_value(with_echo.clone()).expect("echo id should deserialize");
    assert!(matches!(
        &event,
        RuntimeToServerEvent::UserMessageInjected {
            client_echo_id: Some(id),
            ..
        } if id == "echo-终"
    ));
    assert_eq!(
        serde_json::to_value(event).expect("injected message should serialize"),
        with_echo
    );
}

#[test]
fn model_change_missing_or_null_optional_fields_default_to_none_and_serialize_as_null() {
    let canonical = json!({
        "type": "model_changed",
        "provider": "provider",
        "model": "model",
        "thinking_effort": null,
        "context_window": null
    });

    for input in [
        json!({"type": "model_changed", "provider": "provider", "model": "model"}),
        canonical.clone(),
    ] {
        let event: RuntimeToServerEvent =
            serde_json::from_value(input).expect("optional model fields should deserialize");
        assert!(matches!(
            &event,
            RuntimeToServerEvent::ModelChanged {
                thinking_effort: None,
                context_window: None,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_value(event).expect("model event should serialize"),
            canonical
        );
    }
}

#[test]
fn notification_helpers_set_exact_kind_message_and_empty_details() {
    for (event, kind, message) in [
        (
            RuntimeToServerEvent::notice("普通通知"),
            NotificationKind::Info,
            "普通通知",
        ),
        (
            RuntimeToServerEvent::warning("请注意"),
            NotificationKind::Warn,
            "请注意",
        ),
        (
            RuntimeToServerEvent::error("执行失败"),
            NotificationKind::Error,
            "执行失败",
        ),
    ] {
        let RuntimeToServerEvent::Notification(notification) = event else {
            panic!("notification helper should create a notification event");
        };
        assert_eq!(
            notification,
            Notification {
                kind,
                message: message.into(),
                details: Vec::new(),
            }
        );
    }
}

#[test]
fn runtime_events_preserve_empty_text_and_integer_extremes() {
    for (event, expected) in [
        (
            RuntimeToServerEvent::UsageTotalsChanged {
                total_tokens: i64::MIN,
                total_cached_tokens: i64::MAX,
            },
            json!({
                "type": "usage_totals_changed",
                "total_tokens": i64::MIN,
                "total_cached_tokens": i64::MAX
            }),
        ),
        (
            RuntimeToServerEvent::ModelChanged {
                provider: String::new(),
                model: "模型".into(),
                thinking_effort: None,
                context_window: Some(u32::MAX),
            },
            json!({
                "type": "model_changed",
                "provider": "",
                "model": "模型",
                "thinking_effort": null,
                "context_window": u32::MAX
            }),
        ),
        (
            RuntimeToServerEvent::ThreadSwitched {
                from: String::new(),
                to: "线程-终".into(),
            },
            json!({"type": "thread_switched", "from": "", "to": "线程-终"}),
        ),
    ] {
        assert_eq!(
            serde_json::to_value(event).expect("edge event should serialize"),
            expected
        );
    }
}

#[test]
fn runtime_to_server_event_malformed_shapes_are_rejected_with_stable_reasons() {
    for (value, reason) in [
        (json!({}), "missing field `type`"),
        (json!({"type": "unknown"}), "unknown variant `unknown`"),
        (json!({"type": "thinking_delta"}), "missing field `delta`"),
        (
            json!({"type": "thinking_delta", "delta": null}),
            "expected a string",
        ),
        (
            json!({"type": "active_profile_changed", "plan": null}),
            "missing field `profile`",
        ),
        (
            json!({"type": "active_profile_changed", "profile": "unknown"}),
            "unknown variant `unknown`",
        ),
        (
            json!({"type": "user_message_injected"}),
            "missing field `item`",
        ),
        (
            json!({
                "type": "user_message_injected",
                "item": {"type": "message", "role": "user", "content": []},
                "client_echo_id": 1
            }),
            "expected a string",
        ),
        (
            json!({
                "type": "agent_task_event",
                "task_id": "task-1",
                "thread_id": "thread-1",
                "owner_thread_id": "owner-1"
            }),
            "missing field `payload`",
        ),
    ] {
        assert_data_error(value, reason);
    }
}

fn runtime_event_cases() -> Vec<(RuntimeToServerEvent, Value)> {
    let notification = Notification::info("notice");
    let usage = ThreadUsageSnapshot {
        current_context_tokens: 1,
        total_tokens: 2,
        total_cached_tokens: 3,
        context_window: None,
    };
    let tool_use = ToolUseBlock {
        id: "tool-1".into(),
        name: "read".into(),
        input: HashMap::from([("path".into(), json!("src/lib.rs"))]),
    };
    let tool_result = ToolResultBlock {
        tool_use_id: "tool-1".into(),
        is_error: false,
        content: "contents".into(),
        metadata: None,
    };
    let compact_started = CompactEvent {
        trigger: CompactTrigger::Manual,
        thread_id: None,
        agent_label: None,
    };
    let compact_delta = CompactSummaryDeltaEvent {
        trigger: CompactTrigger::Auto,
        delta: "summary".into(),
        thread_id: Some("child-1".into()),
        agent_label: Some("worker".into()),
    };
    let compact_finished = CompactSummaryFinishedEvent {
        trigger: CompactTrigger::Auto,
        summary: "done".into(),
        after_tokens: usize::MAX,
        thread_id: None,
        agent_label: None,
    };
    let compact_failed = CompactSummaryFailedEvent {
        trigger: CompactTrigger::Manual,
        message: "failed".into(),
        thread_id: None,
        agent_label: None,
    };
    let pause = ToolPauseRequest {
        tool_use_id: "tool-2".into(),
        preview_tool_use_id: None,
        tool_name: "request_user_input".into(),
        permission_source: None,
        source_thread_id: None,
        source_agent_label: None,
        kind: ToolPauseKind::UserInput(UserInputPreview {
            questions: Vec::new(),
        }),
    };
    let plan = DisplayPlan {
        id: "plan-1".into(),
        title: "Plan".into(),
        markdown: "- step".into(),
        path: PathBuf::from("/tmp/plan.md"),
        created_at: fixed_time(),
    };
    let task_event = AgentTaskEventEnvelope {
        task_id: "task-1".into(),
        thread_id: "child-1".into(),
        parent_task_id: None,
        owner_thread_id: "owner-1".into(),
        truncated: false,
        payload: AgentTaskEvent::TurnStarted,
    };

    vec![
        (
            RuntimeToServerEvent::RunStarted,
            json!({"type": "run_started"}),
        ),
        (
            RuntimeToServerEvent::UserMessageInjected {
                item: HistoryItem::Message(Message::new(Role::User, Vec::new())),
                client_echo_id: None,
            },
            json!({
                "type": "user_message_injected",
                "item": {"type": "message", "role": "user", "content": []}
            }),
        ),
        (
            RuntimeToServerEvent::RunFinished,
            json!({"type": "run_finished"}),
        ),
        (
            RuntimeToServerEvent::Notification(notification.clone()),
            tagged_payload("notification", &notification),
        ),
        (
            RuntimeToServerEvent::ModelChanged {
                provider: "provider".into(),
                model: "model".into(),
                thinking_effort: Some(omini_domain::config::ThinkingEffort::High),
                context_window: Some(128_000),
            },
            json!({
                "type": "model_changed",
                "provider": "provider",
                "model": "model",
                "thinking_effort": "high",
                "context_window": 128_000
            }),
        ),
        (
            RuntimeToServerEvent::UsageChanged(usage),
            tagged_payload("usage_changed", usage),
        ),
        (
            RuntimeToServerEvent::UsageTotalsChanged {
                total_tokens: 10,
                total_cached_tokens: 4,
            },
            json!({
                "type": "usage_totals_changed",
                "total_tokens": 10,
                "total_cached_tokens": 4
            }),
        ),
        (
            RuntimeToServerEvent::ActiveProfileChanged(ActiveProfile::Auto),
            json!({"type": "active_profile_changed", "profile": "auto"}),
        ),
        (
            RuntimeToServerEvent::AgentManagementUpdated {
                records: Vec::new(),
            },
            json!({"type": "agent_management_updated", "records": []}),
        ),
        (
            RuntimeToServerEvent::TurnStarted,
            json!({"type": "turn_started"}),
        ),
        (
            RuntimeToServerEvent::TurnEnded,
            json!({"type": "turn_ended"}),
        ),
        (
            RuntimeToServerEvent::ThinkingDelta("thinking".into()),
            json!({"type": "thinking_delta", "delta": "thinking"}),
        ),
        (
            RuntimeToServerEvent::TextDelta("text".into()),
            json!({"type": "text_delta", "delta": "text"}),
        ),
        (
            RuntimeToServerEvent::ProposedPlanDelta("plan".into()),
            json!({"type": "proposed_plan_delta", "delta": "plan"}),
        ),
        (
            RuntimeToServerEvent::ToolUse(tool_use.clone()),
            tagged_payload("tool_use", &tool_use),
        ),
        (
            RuntimeToServerEvent::ToolResult(tool_result.clone()),
            tagged_payload("tool_result", &tool_result),
        ),
        (
            RuntimeToServerEvent::CompactSummaryStarted(compact_started.clone()),
            tagged_payload("compact_summary_started", &compact_started),
        ),
        (
            RuntimeToServerEvent::CompactSummaryDelta(compact_delta.clone()),
            tagged_payload("compact_summary_delta", &compact_delta),
        ),
        (
            RuntimeToServerEvent::CompactSummaryFinished(compact_finished.clone()),
            tagged_payload("compact_summary_finished", &compact_finished),
        ),
        (
            RuntimeToServerEvent::CompactSummaryFailed(compact_failed.clone()),
            tagged_payload("compact_summary_failed", &compact_failed),
        ),
        (
            RuntimeToServerEvent::ToolPauseRequested(pause.clone()),
            tagged_payload("tool_pause_requested", &pause),
        ),
        (
            RuntimeToServerEvent::PlanSubmitted(plan.clone()),
            tagged_payload("plan_submitted", &plan),
        ),
        (
            RuntimeToServerEvent::PlanApprovalResolved {
                plan_id: "plan-1".into(),
                action: PlanApprovalAction::ApproveInNewThread {
                    profile: PlanExecutionProfile::Main,
                },
            },
            json!({
                "type": "plan_approval_resolved",
                "plan_id": "plan-1",
                "action": {"type": "approve_in_new_thread", "profile": "main"}
            }),
        ),
        (
            RuntimeToServerEvent::ThreadSwitched {
                from: "thread-1".into(),
                to: "thread-2".into(),
            },
            json!({"type": "thread_switched", "from": "thread-1", "to": "thread-2"}),
        ),
        (
            RuntimeToServerEvent::AgentTaskEvent(task_event.clone()),
            tagged_payload("agent_task_event", &task_event),
        ),
    ]
}

fn runtime_event_type(event: &RuntimeToServerEvent) -> &'static str {
    match event {
        RuntimeToServerEvent::RunStarted => "run_started",
        RuntimeToServerEvent::UserMessageInjected { .. } => "user_message_injected",
        RuntimeToServerEvent::RunFinished => "run_finished",
        RuntimeToServerEvent::Notification(_) => "notification",
        RuntimeToServerEvent::ModelChanged { .. } => "model_changed",
        RuntimeToServerEvent::UsageChanged(_) => "usage_changed",
        RuntimeToServerEvent::UsageTotalsChanged { .. } => "usage_totals_changed",
        RuntimeToServerEvent::ActiveProfileChanged(_) => "active_profile_changed",
        RuntimeToServerEvent::AgentManagementUpdated { .. } => "agent_management_updated",
        RuntimeToServerEvent::TurnStarted => "turn_started",
        RuntimeToServerEvent::TurnEnded => "turn_ended",
        RuntimeToServerEvent::ThinkingDelta(_) => "thinking_delta",
        RuntimeToServerEvent::TextDelta(_) => "text_delta",
        RuntimeToServerEvent::ProposedPlanDelta(_) => "proposed_plan_delta",
        RuntimeToServerEvent::ToolUse(_) => "tool_use",
        RuntimeToServerEvent::ToolResult(_) => "tool_result",
        RuntimeToServerEvent::CompactSummaryStarted(_) => "compact_summary_started",
        RuntimeToServerEvent::CompactSummaryDelta(_) => "compact_summary_delta",
        RuntimeToServerEvent::CompactSummaryFinished(_) => "compact_summary_finished",
        RuntimeToServerEvent::CompactSummaryFailed(_) => "compact_summary_failed",
        RuntimeToServerEvent::ToolPauseRequested(_) => "tool_pause_requested",
        RuntimeToServerEvent::PlanSubmitted(_) => "plan_submitted",
        RuntimeToServerEvent::PlanApprovalResolved { .. } => "plan_approval_resolved",
        RuntimeToServerEvent::ThreadSwitched { .. } => "thread_switched",
        RuntimeToServerEvent::AgentTaskEvent(_) => "agent_task_event",
    }
}

// 只由 runtime-contract 补上顶层事件 tag，嵌套 DTO 的字段形状继续由 omini-domain 自己保障。
fn tagged_payload(tag: &str, payload: impl Serialize) -> Value {
    let Value::Object(mut object) =
        serde_json::to_value(payload).expect("payload should serialize as an object")
    else {
        panic!("runtime event payload should be an object");
    };
    object.insert("type".into(), json!(tag));
    Value::Object(object)
}

fn assert_data_error(value: Value, reason: &str) {
    let error = serde_json::from_value::<RuntimeToServerEvent>(value)
        .expect_err("malformed runtime event should be rejected");
    assert_eq!(error.classify(), serde_json::error::Category::Data);
    assert!(
        error.to_string().contains(reason),
        "error {error:?} should contain {reason:?}"
    );
}

fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 12, 0, 0, 0)
        .single()
        .expect("fixed test time should be valid")
}
