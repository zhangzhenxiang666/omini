use omini_protocol::{
    AckResponse, AgentRecord, AgentSourceKind, CreateProjectRequest, CreateThreadRequest,
    GenerateAgentRequest, GenerateAgentResponse, ProjectPathStatus, ProtocolError, SetModelRequest,
    SetThinkingDisplayRequest,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[test]
fn register_client_request_empty_body_uses_default_kind() {
    let request: omini_protocol::RegisterClientRequest =
        serde_json::from_value(json!({})).expect("empty registration request should be accepted");

    assert_eq!(request.kind, None);
    assert_eq!(serde_json::to_value(request).unwrap(), json!({}));
}

#[test]
fn create_project_request_without_name_omits_name() {
    let request = CreateProjectRequest {
        path: "/work/project".to_string(),
        name: None,
    };

    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({ "path": "/work/project" })
    );
}

#[test]
fn project_path_status_variants_use_stable_wire_names() {
    for (status, expected) in [
        (ProjectPathStatus::Ready, "ready"),
        (ProjectPathStatus::Missing, "missing"),
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), json!(expected));
    }
}

#[test]
fn project_path_status_unknown_value_is_data_error() {
    assert_data_error::<ProjectPathStatus>(json!("stale"), "unknown variant");
}

#[test]
fn create_thread_request_missing_overrides_uses_defaults() {
    let request: CreateThreadRequest =
        serde_json::from_value(json!({})).expect("thread creation should inherit project defaults");

    assert_eq!(request, CreateThreadRequest::default());
    assert_eq!(serde_json::to_value(request).unwrap(), json!({}));
}

#[test]
fn create_thread_request_partial_model_override_preserves_missing_model() {
    let request: CreateThreadRequest = serde_json::from_value(json!({ "provider": "openai" }))
        .expect("server resolves partial model overrides against project settings");

    assert_eq!(request.provider.as_deref(), Some("openai"));
    assert_eq!(request.model, None);
    assert_eq!(request.thinking_effort, None);
    assert_eq!(request.profile, None);
}

#[test]
fn model_and_display_requests_omit_unset_optional_fields() {
    let model = SetModelRequest {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        thinking_effort: None,
    };
    let display: SetThinkingDisplayRequest = serde_json::from_value(json!({ "show": null }))
        .expect("null display preference should retain toggle semantics");

    assert_eq!(
        serde_json::to_value(model).unwrap(),
        json!({ "provider": "openai", "model": "gpt-test" })
    );
    assert_eq!(display.show, None);
    assert_eq!(serde_json::to_value(display).unwrap(), json!({}));
}

#[test]
fn agent_generation_request_missing_model_is_data_error() {
    assert_data_error::<GenerateAgentRequest>(
        json!({ "description": "review", "provider": "openai" }),
        "missing field",
    );
}

#[test]
fn agent_generation_response_omits_optional_short_description() {
    let response = GenerateAgentResponse {
        draft: omini_protocol::GeneratedAgentDraft {
            name: "reviewer".to_string(),
            description: "Reviews changes".to_string(),
            short_description: None,
            instructions: "Inspect the diff.".to_string(),
        },
    };

    assert_eq!(
        serde_json::to_value(response).unwrap(),
        json!({
            "draft": {
                "name": "reviewer",
                "description": "Reviews changes",
                "instructions": "Inspect the diff."
            }
        })
    );
}

#[test]
fn agent_record_empty_tool_policies_are_omitted() {
    let record = AgentRecord {
        id: "built-in:reviewer".to_string(),
        name: "reviewer".to_string(),
        description: "Reviews changes".to_string(),
        short_description: None,
        instructions: "Inspect the diff.".to_string(),
        tools: Vec::new(),
        disallow_tools: Vec::new(),
        model: None,
        source_kind: AgentSourceKind::BuiltIn,
        editable: false,
    };

    let value = serde_json::to_value(record).unwrap();
    assert_eq!(value["source_kind"], json!("built_in"));
    assert_eq!(value["editable"], json!(false));
    assert!(value.get("tools").is_none());
    assert!(value.get("disallow_tools").is_none());
    assert!(value.get("model").is_none());
}

#[test]
fn protocol_response_constructors_preserve_error_and_success_values() {
    let error = ProtocolError::new("invalid_model", "model is unavailable");

    assert_eq!(
        serde_json::to_value(error).unwrap(),
        json!({ "code": "invalid_model", "message": "model is unavailable" })
    );
    assert_eq!(AckResponse::ok(), AckResponse { ok: true });
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
