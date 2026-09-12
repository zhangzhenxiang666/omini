use omini_protocol::{InputPart, RunCommand, SubmitRunRequest, UserInput};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[test]
fn input_parts_preserve_exact_order_and_wire_tags() {
    let input = UserInput {
        parts: vec![
            InputPart::Text {
                text: "before ".to_string(),
            },
            InputPart::Skill {
                name: "review".to_string(),
            },
            InputPart::File {
                path: "src/main.rs".to_string(),
                label: Some("main".to_string()),
            },
            InputPart::Directory {
                path: "src".to_string(),
                label: None,
            },
            InputPart::Subagent {
                name: "explorer".to_string(),
                label: Some("research".to_string()),
            },
            InputPart::Text {
                text: " after".to_string(),
            },
        ],
        attachment_ids: vec!["att-b".to_string(), "att-a".to_string()],
    };
    let wire = serde_json::to_value(&input).unwrap();
    assert_eq!(
        wire,
        json!({
            "parts": [
                {"type": "text", "text": "before "},
                {"type": "skill", "name": "review"},
                {"type": "file", "path": "src/main.rs", "label": "main"},
                {"type": "directory", "path": "src"},
                {"type": "subagent", "name": "explorer", "label": "research"},
                {"type": "text", "text": " after"}
            ],
            "attachment_ids": ["att-b", "att-a"]
        })
    );
    assert_eq!(serde_json::from_value::<UserInput>(wire).unwrap(), input);
}

#[test]
fn submit_intervene_and_init_use_exact_tagged_shapes() {
    let input = UserInput::plain("notes".to_string());
    let cases = [
        (
            SubmitRunRequest::SubmitMessage {
                input: input.clone(),
                client_echo_id: Some("echo-1".to_string()),
            },
            json!({
                "type": "submit_message",
                "input": {"parts": [{"type": "text", "text": "notes"}]},
                "client_echo_id": "echo-1"
            }),
        ),
        (
            SubmitRunRequest::InterveneMessage {
                input: input.clone(),
                client_echo_id: None,
            },
            json!({
                "type": "intervene_message",
                "input": {"parts": [{"type": "text", "text": "notes"}]}
            }),
        ),
        (
            SubmitRunRequest::ExecuteCommand {
                command: RunCommand::Init,
                input,
                client_echo_id: None,
            },
            json!({
                "type": "execute_command",
                "command": {"type": "init"},
                "input": {"parts": [{"type": "text", "text": "notes"}]}
            }),
        ),
    ];

    for (request, wire) in cases {
        assert_eq!(serde_json::to_value(&request).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<SubmitRunRequest>(wire).unwrap(),
            request
        );
    }
}

#[test]
fn unknown_variants_and_legacy_shape_are_rejected() {
    assert_data_error::<InputPart>(json!({"type": "url", "url": "https://example.test"}));
    assert_data_error::<SubmitRunRequest>(json!({
        "type": "unknown",
        "input": {"parts": [], "attachment_ids": []}
    }));
    assert_data_error::<SubmitRunRequest>(json!({
        "text": "legacy",
        "context_refs": [],
        "attachments": []
    }));
    assert_data_error::<SubmitRunRequest>(json!({
        "type": "submit_message",
        "input": {
            "parts": [{"type": "text", "text": "new"}],
            "text": "legacy"
        }
    }));
}

#[test]
fn protocol_revision_is_two() {
    assert_eq!(omini_protocol::PROTOCOL_REVISION, 2);
}

fn assert_data_error<T>(value: Value)
where
    T: DeserializeOwned + std::fmt::Debug,
{
    let error = serde_json::from_value::<T>(value).expect_err("invalid shape must fail");
    assert!(error.is_data(), "expected data error, got: {error}");
}
