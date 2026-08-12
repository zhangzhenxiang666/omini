use omini_domain::config::ThinkingEffort;
use omini_domain::display::UserDraft;
use omini_domain::events::{
    ActiveProfile, PlanApprovalAction, PlanExecutionProfile, ToolPauseResponse,
};
use omini_runtime_contract::ServerToRuntimeEvent;
use serde_json::{Value, json};

#[test]
fn server_to_runtime_event_all_variants_keep_their_tagged_contract() {
    let cases = [
        (
            ServerToRuntimeEvent::CancelRun,
            json!({"type": "cancel_run"}),
        ),
        (
            ServerToRuntimeEvent::SendMessage {
                draft: UserDraft::plain("hello".into()),
                client_echo_id: None,
            },
            json!({
                "type": "send_message",
                "draft": {"text": "hello", "mentions": [], "images": []}
            }),
        ),
        (
            ServerToRuntimeEvent::CompactContext {
                instructions: Some("focus".into()),
            },
            json!({"type": "compact_context", "instructions": "focus"}),
        ),
        (
            ServerToRuntimeEvent::SetThinkingEffort(ThinkingEffort::High),
            json!({"type": "set_thinking_effort", "effort": "high"}),
        ),
        (
            ServerToRuntimeEvent::ToggleActiveProfile,
            json!({"type": "toggle_active_profile"}),
        ),
        (
            ServerToRuntimeEvent::SetActiveProfile(ActiveProfile::Plan),
            json!({"type": "set_active_profile", "profile": "plan"}),
        ),
        (
            ServerToRuntimeEvent::InterveneMessage {
                draft: UserDraft::plain("补充说明".into()),
                client_echo_id: Some("echo-1".into()),
            },
            json!({
                "type": "intervene_message",
                "draft": {"text": "补充说明", "mentions": [], "images": []},
                "client_echo_id": "echo-1"
            }),
        ),
        (
            ServerToRuntimeEvent::ModelSelected {
                provider: "provider".into(),
                model: "model".into(),
                thinking_effort: Some(ThinkingEffort::XHigh),
            },
            json!({
                "type": "model_selected",
                "provider": "provider",
                "model": "model",
                "thinking_effort": "xhigh"
            }),
        ),
        (
            ServerToRuntimeEvent::CloseRuntime,
            json!({"type": "close_runtime"}),
        ),
        (
            ServerToRuntimeEvent::SubagentRegistryChanged,
            json!({"type": "subagent_registry_changed"}),
        ),
        (
            ServerToRuntimeEvent::ResolveToolPause {
                tool_use_id: "tool-1".into(),
                response: ToolPauseResponse::Permission {
                    approved: false,
                    note: None,
                },
            },
            json!({
                "type": "resolve_tool_pause",
                "tool_use_id": "tool-1",
                "response": {"type": "permission", "approved": false}
            }),
        ),
        (
            ServerToRuntimeEvent::ResolvePlanApproval {
                plan_id: "plan-1".into(),
                action: PlanApprovalAction::Approve {
                    profile: PlanExecutionProfile::Auto,
                },
            },
            json!({
                "type": "resolve_plan_approval",
                "plan_id": "plan-1",
                "action": {"type": "approve", "profile": "auto"}
            }),
        ),
    ];

    for (event, expected) in cases {
        assert_eq!(server_event_type(&event), expected["type"]);
        assert_eq!(
            serde_json::to_value(&event).expect("server event should serialize"),
            expected
        );

        let decoded: ServerToRuntimeEvent = serde_json::from_value(expected.clone())
            .expect("canonical server event should deserialize");
        assert_eq!(server_event_type(&decoded), expected["type"]);
        assert_eq!(
            serde_json::to_value(decoded).expect("decoded server event should serialize"),
            expected
        );
    }
}

#[test]
fn scalar_events_all_values_use_named_payload_fields() {
    for (effort, name) in [
        (ThinkingEffort::None, "none"),
        (ThinkingEffort::Low, "low"),
        (ThinkingEffort::Medium, "medium"),
        (ThinkingEffort::High, "high"),
        (ThinkingEffort::XHigh, "xhigh"),
        (ThinkingEffort::Max, "max"),
    ] {
        let expected = json!({"type": "set_thinking_effort", "effort": name});
        let event = ServerToRuntimeEvent::SetThinkingEffort(effort);
        assert_eq!(
            serde_json::to_value(event).expect("thinking effort should serialize"),
            expected
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<ServerToRuntimeEvent>(expected.clone())
                    .expect("thinking effort should deserialize")
            )
            .expect("thinking effort should reserialize"),
            expected
        );
    }

    for (profile, name) in [
        (ActiveProfile::Main, "main"),
        (ActiveProfile::Auto, "auto"),
        (ActiveProfile::Plan, "plan"),
    ] {
        let expected = json!({"type": "set_active_profile", "profile": name});
        let event = ServerToRuntimeEvent::SetActiveProfile(profile);
        assert_eq!(
            serde_json::to_value(event).expect("active profile should serialize"),
            expected
        );
        assert_eq!(
            serde_json::to_value(
                serde_json::from_value::<ServerToRuntimeEvent>(expected.clone())
                    .expect("active profile should deserialize")
            )
            .expect("active profile should reserialize"),
            expected
        );
    }
}

#[test]
fn message_events_missing_or_null_echo_id_default_to_none_and_are_omitted() {
    for event_type in ["send_message", "intervene_message"] {
        let canonical = json!({
            "type": event_type,
            "draft": {"text": "", "mentions": [], "images": []}
        });

        for input in [
            canonical.clone(),
            json!({
                "type": event_type,
                "draft": {"text": "", "mentions": [], "images": []},
                "client_echo_id": null
            }),
        ] {
            let event: ServerToRuntimeEvent =
                serde_json::from_value(input).expect("absent echo id should deserialize");
            match &event {
                ServerToRuntimeEvent::SendMessage { client_echo_id, .. }
                | ServerToRuntimeEvent::InterveneMessage { client_echo_id, .. } => {
                    assert_eq!(client_echo_id, &None);
                }
                _ => panic!("expected a message event"),
            }
            assert_eq!(
                serde_json::to_value(event).expect("message event should serialize"),
                canonical
            );
        }
    }
}

#[test]
fn nullable_fields_missing_or_null_default_to_none_and_serialize_as_null() {
    for (input, canonical) in [
        (
            json!({"type": "compact_context"}),
            json!({"type": "compact_context", "instructions": null}),
        ),
        (
            json!({"type": "compact_context", "instructions": null}),
            json!({"type": "compact_context", "instructions": null}),
        ),
        (
            json!({"type": "model_selected", "provider": "p", "model": "m"}),
            json!({
                "type": "model_selected",
                "provider": "p",
                "model": "m",
                "thinking_effort": null
            }),
        ),
        (
            json!({
                "type": "model_selected",
                "provider": "p",
                "model": "m",
                "thinking_effort": null
            }),
            json!({
                "type": "model_selected",
                "provider": "p",
                "model": "m",
                "thinking_effort": null
            }),
        ),
    ] {
        let event: ServerToRuntimeEvent =
            serde_json::from_value(input).expect("nullable field should deserialize");
        assert_eq!(
            serde_json::to_value(event).expect("nullable field should serialize"),
            canonical
        );
    }
}

#[test]
fn server_to_runtime_event_malformed_shapes_are_rejected_with_stable_reasons() {
    for (value, reason) in [
        (json!({}), "missing field `type`"),
        (json!({"type": "unknown"}), "unknown variant `unknown`"),
        (
            json!({"type": "set_thinking_effort"}),
            "missing field `effort`",
        ),
        (
            json!({"type": "set_thinking_effort", "effort": "maximum"}),
            "unknown variant `maximum`",
        ),
        (
            json!({"type": "set_thinking_effort", "high": null}),
            "missing field `effort`",
        ),
        (
            json!({"type": "set_active_profile", "plan": null}),
            "missing field `profile`",
        ),
        (json!({"type": "send_message"}), "missing field `draft`"),
        (
            json!({
                "type": "send_message",
                "draft": {"text": "hello", "mentions": [], "images": []},
                "client_echo_id": 1
            }),
            "expected a string",
        ),
        (
            json!({
                "type": "resolve_plan_approval",
                "plan_id": "plan-1",
                "action": {"type": "reject"}
            }),
            "unknown variant `reject`",
        ),
    ] {
        assert_data_error(value, reason);
    }
}

fn server_event_type(event: &ServerToRuntimeEvent) -> &'static str {
    match event {
        ServerToRuntimeEvent::CancelRun => "cancel_run",
        ServerToRuntimeEvent::SendMessage { .. } => "send_message",
        ServerToRuntimeEvent::CompactContext { .. } => "compact_context",
        ServerToRuntimeEvent::SetThinkingEffort(_) => "set_thinking_effort",
        ServerToRuntimeEvent::ToggleActiveProfile => "toggle_active_profile",
        ServerToRuntimeEvent::SetActiveProfile(_) => "set_active_profile",
        ServerToRuntimeEvent::InterveneMessage { .. } => "intervene_message",
        ServerToRuntimeEvent::ModelSelected { .. } => "model_selected",
        ServerToRuntimeEvent::CloseRuntime => "close_runtime",
        ServerToRuntimeEvent::SubagentRegistryChanged => "subagent_registry_changed",
        ServerToRuntimeEvent::ResolveToolPause { .. } => "resolve_tool_pause",
        ServerToRuntimeEvent::ResolvePlanApproval { .. } => "resolve_plan_approval",
    }
}

fn assert_data_error(value: Value, reason: &str) {
    let error = serde_json::from_value::<ServerToRuntimeEvent>(value)
        .expect_err("malformed server event should be rejected");
    assert_eq!(error.classify(), serde_json::error::Category::Data);
    assert!(
        error.to_string().contains(reason),
        "error {error:?} should contain {reason:?}"
    );
}
