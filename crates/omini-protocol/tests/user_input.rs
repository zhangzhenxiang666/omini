use omini_protocol::{AttachmentRef, ContextRef, RunInputMode, SubmitRunRequest, UserInput};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[test]
fn context_ref_variants_preserve_tags_targets_and_labels() {
    let cases = [
        (
            ContextRef::File {
                path: "src/main.rs".to_string(),
                label: None,
            },
            "src/main.rs",
            "src/main.rs",
            json!({ "kind": "file", "path": "src/main.rs" }),
        ),
        (
            ContextRef::Directory {
                path: "src".to_string(),
                label: Some("source".to_string()),
            },
            "src",
            "source",
            json!({ "kind": "directory", "path": "src", "label": "source" }),
        ),
        (
            ContextRef::Subagent {
                name: "explorer".to_string(),
                label: None,
            },
            "explorer",
            "explorer",
            json!({ "kind": "subagent", "name": "explorer" }),
        ),
        (
            ContextRef::Url {
                url: "https://example.test/docs".to_string(),
                label: Some("docs".to_string()),
            },
            "https://example.test/docs",
            "docs",
            json!({ "kind": "url", "url": "https://example.test/docs", "label": "docs" }),
        ),
    ];

    for (reference, target, label, wire) in cases {
        assert_eq!(reference.target(), target);
        assert_eq!(reference.label(), label);
        assert_eq!(serde_json::to_value(&reference).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ContextRef>(wire).unwrap(),
            reference
        );
    }
}

#[test]
fn context_ref_unknown_kind_is_data_error() {
    assert_data_error::<ContextRef>(
        json!({ "kind": "command", "value": "ls" }),
        "unknown variant",
    );
}

#[test]
fn attachment_variants_preserve_identity_and_optional_metadata() {
    let local = AttachmentRef::LocalPath {
        path: "/tmp/diagram.png".to_string(),
        mime_type: None,
        name: None,
    };
    let uploaded = AttachmentRef::Uploaded {
        attachment_id: "att_1".to_string(),
        mime_type: "image/png".to_string(),
        name: Some("diagram.png".to_string()),
    };

    assert_eq!(local.name(), None);
    assert_eq!(local.mime_type(), None);
    assert_eq!(
        serde_json::to_value(&local).unwrap(),
        json!({ "kind": "local_path", "path": "/tmp/diagram.png" })
    );
    assert_eq!(uploaded.name(), Some("diagram.png"));
    assert_eq!(uploaded.mime_type(), Some("image/png"));
    assert_eq!(
        serde_json::to_value(&uploaded).unwrap(),
        json!({
            "kind": "uploaded",
            "attachment_id": "att_1",
            "mime_type": "image/png",
            "name": "diagram.png"
        })
    );
}

#[test]
fn uploaded_attachment_missing_mime_type_is_data_error() {
    assert_data_error::<AttachmentRef>(
        json!({ "kind": "uploaded", "attachment_id": "att_1" }),
        "missing field",
    );
}

#[test]
fn submit_run_request_preserves_complete_input_and_mode() {
    let request = SubmitRunRequest {
        input: UserInput {
            text: "review @main".to_string(),
            context_refs: Some(vec![ContextRef::File {
                path: "src/main.rs".to_string(),
                label: Some("main".to_string()),
            }]),
            attachments: Some(vec![AttachmentRef::Uploaded {
                attachment_id: "att_1".to_string(),
                mime_type: "image/png".to_string(),
                name: None,
            }]),
        },
        client_echo_id: Some("echo_1".to_string()),
        mode: RunInputMode::Intervene,
    };

    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(
        wire,
        json!({
            "input": {
                "text": "review @main",
                "context_refs": [{ "kind": "file", "path": "src/main.rs", "label": "main" }],
                "attachments": [{ "kind": "uploaded", "attachment_id": "att_1", "mime_type": "image/png" }]
            },
            "client_echo_id": "echo_1",
            "mode": "intervene"
        })
    );
    assert_eq!(
        serde_json::from_value::<SubmitRunRequest>(wire).unwrap(),
        request
    );
}

#[test]
fn run_input_mode_variants_use_stable_wire_names() {
    for (mode, expected) in [
        (RunInputMode::Submit, "submit"),
        (RunInputMode::Intervene, "intervene"),
    ] {
        assert_eq!(serde_json::to_value(mode).unwrap(), json!(expected));
    }
}

#[test]
fn submit_run_request_missing_mode_is_data_error() {
    assert_data_error::<SubmitRunRequest>(json!({ "input": { "text": "hello" } }), "missing field");
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
