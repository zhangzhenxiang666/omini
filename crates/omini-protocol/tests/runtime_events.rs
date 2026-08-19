use omini_protocol::{
    ClientThreadRole, NotificationLevel, RuntimeEvent, ServerEnvelope, ThreadSwitchedEvent,
    TypedRuntimeEvent,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[test]
fn typed_runtime_event_variants_report_their_wire_kind() {
    for (kind, payload) in typed_event_payloads() {
        let event: TypedRuntimeEvent = serde_json::from_value(payload)
            .expect("representative payload should decode through the public protocol");

        assert_eq!(event.kind(), kind);
        assert_eq!(RuntimeEvent::new(event).kind(), kind);
    }
}

#[test]
fn typed_runtime_event_unknown_type_is_data_error() {
    assert_data_error::<TypedRuntimeEvent>(json!({ "type": "run_paused" }), "unknown variant");
}

#[test]
fn thread_snapshot_event_missing_thread_id_is_data_error() {
    assert_data_error::<TypedRuntimeEvent>(
        json!({
            "type": "thread_snapshot",
            "messages": [],
            "agent_tasks": [],
            "usage": {
                "current_context_tokens": 0,
                "total_tokens": 0,
                "total_cached_tokens": 0,
                "context_window": null
            }
        }),
        "missing field",
    );
}

#[test]
fn agent_task_event_preserves_nested_tag_and_optional_parent() {
    let wire = json!({
        "type": "agent_task_event",
        "task_id": "task_1",
        "thread_id": "thread_1",
        "parent_task_id": "task_parent",
        "owner_thread_id": "owner_1",
        "payload": { "type": "text_delta", "delta": "hello" }
    });
    let event: TypedRuntimeEvent = serde_json::from_value(wire.clone()).unwrap();

    assert_eq!(serde_json::to_value(event).unwrap(), wire);
}

#[test]
fn thread_switched_event_round_trips_inside_runtime_envelope() {
    // server 通过普通 runtime 通道广播切换；TUI 会在投递 UI 状态机前拦截它并重连新线程。
    let event = RuntimeEvent::new(TypedRuntimeEvent::ThreadSwitched(ThreadSwitchedEvent {
        from: "thread_old".to_string(),
        to: "thread_new".to_string(),
    }));
    let envelope = ServerEnvelope::Event {
        event: event.clone(),
    };
    let wire = serde_json::to_value(envelope).unwrap();

    assert_eq!(
        wire,
        json!({
            "type": "event",
            "event": {
                "event": {
                    "type": "thread_switched",
                    "from": "thread_old",
                    "to": "thread_new"
                }
            }
        })
    );
    let decoded: ServerEnvelope = serde_json::from_value(wire).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(ServerEnvelope::Event { event }).unwrap()
    );
}

#[test]
fn server_envelope_controller_updates_preserve_role_and_absence() {
    let controller = ServerEnvelope::ControllerChanged {
        controller_id: None,
    };
    let role = ServerEnvelope::ClientRoleChanged {
        client_id: "client_1".to_string(),
        role: ClientThreadRole::Observer,
        controller_id: None,
    };

    assert_eq!(
        serde_json::to_value(controller).unwrap(),
        json!({ "type": "controller_changed", "controller_id": null })
    );
    assert_eq!(
        serde_json::to_value(role).unwrap(),
        json!({ "type": "client_role_changed", "client_id": "client_1", "role": "observer" })
    );
}

#[test]
fn client_thread_role_variants_use_stable_wire_names() {
    for (role, expected) in [
        (ClientThreadRole::Controller, "controller"),
        (ClientThreadRole::Observer, "observer"),
    ] {
        assert_eq!(serde_json::to_value(role).unwrap(), json!(expected));
    }
}

#[test]
fn notification_level_variants_use_stable_wire_names() {
    for (level, expected) in [
        (NotificationLevel::Info, "info"),
        (NotificationLevel::Warn, "warn"),
        (NotificationLevel::Error, "error"),
    ] {
        assert_eq!(serde_json::to_value(level).unwrap(), json!(expected));
    }
}

#[test]
fn server_envelope_unknown_type_is_data_error() {
    assert_data_error::<ServerEnvelope>(json!({ "type": "ping" }), "unknown variant");
}

fn typed_event_payloads() -> Vec<(&'static str, Value)> {
    vec![
        ("run_started", json!({ "type": "run_started" })),
        (
            "user_message_injected",
            json!({
                "type": "user_message_injected",
                "item": { "type": "message", "role": "user", "content": [] },
                "client_echo_id": "echo_1"
            }),
        ),
        ("run_finished", json!({ "type": "run_finished" })),
        (
            "notification",
            json!({ "type": "notification", "level": "warn", "message": "lagged", "details": ["retry"] }),
        ),
        (
            "model_changed",
            json!({ "type": "model_changed", "provider": "openai", "model": "gpt-test" }),
        ),
        (
            "thinking_display_changed",
            json!({ "type": "thinking_display_changed", "show": true }),
        ),
        (
            "usage_changed",
            json!({
                "type": "usage_changed",
                "current_context_tokens": 1,
                "total_tokens": 2,
                "total_cached_tokens": 3,
                "context_window": 4
            }),
        ),
        (
            "usage_totals_changed",
            json!({ "type": "usage_totals_changed", "total_tokens": 2, "total_cached_tokens": 3 }),
        ),
        (
            "active_profile_changed",
            json!({ "type": "active_profile_changed", "profile": "plan" }),
        ),
        (
            "thread_title_changed",
            json!({ "type": "thread_title_changed", "title": null }),
        ),
        (
            "tool_pause_requested",
            json!({
                "type": "tool_pause_requested",
                "tool_use_id": "tool_1",
                "tool_name": "bash",
                "kind": { "type": "permission", "preview": { "type": "custom", "tool_name": "bash", "payload": {} } }
            }),
        ),
        (
            "plan_submitted",
            json!({
                "type": "plan_submitted",
                "id": "plan_1",
                "title": "Plan",
                "markdown": "# Plan",
                "path": "plans/plan.md",
                "created_at": "2024-01-02T03:04:05Z"
            }),
        ),
        (
            "plan_approval_resolved",
            json!({
                "type": "plan_approval_resolved",
                "plan_id": "plan_1",
                "action": { "type": "approve_in_new_thread", "profile": "auto" }
            }),
        ),
        (
            "agent_management_updated",
            json!({ "type": "agent_management_updated", "records": [] }),
        ),
        ("turn_started", json!({ "type": "turn_started" })),
        ("turn_ended", json!({ "type": "turn_ended" })),
        (
            "git_branch_changed",
            json!({ "type": "git_branch_changed", "branch": null }),
        ),
        (
            "thinking_delta",
            json!({ "type": "thinking_delta", "delta": "reasoning" }),
        ),
        (
            "text_delta",
            json!({ "type": "text_delta", "delta": "answer" }),
        ),
        (
            "proposed_plan_delta",
            json!({ "type": "proposed_plan_delta", "delta": "step" }),
        ),
        (
            "tool_use",
            json!({ "type": "tool_use", "id": "tool_1", "name": "bash", "input": {} }),
        ),
        (
            "tool_result",
            json!({ "type": "tool_result", "tool_use_id": "tool_1", "is_error": false, "content": "ok" }),
        ),
        (
            "compact_summary_started",
            json!({ "type": "compact_summary_started", "trigger": "manual" }),
        ),
        (
            "compact_summary_delta",
            json!({ "type": "compact_summary_delta", "trigger": "auto", "delta": "partial" }),
        ),
        (
            "compact_summary_finished",
            json!({ "type": "compact_summary_finished", "trigger": "manual", "summary": "done", "after_tokens": 5 }),
        ),
        (
            "compact_summary_failed",
            json!({ "type": "compact_summary_failed", "trigger": "auto", "message": "failed" }),
        ),
        (
            "thread_snapshot",
            json!({
                "type": "thread_snapshot",
                "thread_id": "thread_1",
                "messages": [],
                "agent_tasks": [],
                "usage": {
                    "current_context_tokens": 1,
                    "total_tokens": 2,
                    "total_cached_tokens": 3,
                    "context_window": null
                }
            }),
        ),
        (
            "thread_switched",
            json!({ "type": "thread_switched", "from": "thread_old", "to": "thread_new" }),
        ),
        (
            "agent_task_event",
            json!({
                "type": "agent_task_event",
                "task_id": "task_1",
                "thread_id": "thread_1",
                "owner_thread_id": "owner_1",
                "payload": { "type": "turn_started" }
            }),
        ),
    ]
}

fn assert_data_error<T>(value: Value, reason: &str)
where
    T: DeserializeOwned,
{
    let error = match serde_json::from_value::<T>(value) {
        Ok(_) => panic!("invalid protocol shape must fail"),
        Err(error) => error,
    };
    assert!(error.is_data(), "expected data error, got: {error}");
    assert!(
        error.to_string().contains(reason),
        "unexpected error: {error}"
    );
}
