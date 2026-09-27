use omini_protocol as wire;
use omini_runtime_contract::thread_domain as runtime;

#[test]
fn tool_pause_conversion_preserves_permission_and_user_input_wire() {
    let requests = [
        runtime::ToolPauseRequest {
            tool_use_id: "tool-1".to_string(),
            preview_tool_use_id: Some("preview-1".to_string()),
            tool_name: "bash".to_string(),
            permission_source: Some(runtime::PermissionSource {
                decision: "ask".to_string(),
                source: "policy".to_string(),
                rule: "bash".to_string(),
            }),
            source_thread_id: Some("agent-1".to_string()),
            source_agent_label: Some("explorer".to_string()),
            kind: runtime::ToolPauseKind::Permission(runtime::PermissionPreview::Custom {
                tool_name: "bash".to_string(),
                payload: serde_json::Map::from_iter([(
                    "command".to_string(),
                    serde_json::json!("pwd"),
                )]),
            }),
        },
        runtime::ToolPauseRequest {
            tool_use_id: "tool-2".to_string(),
            preview_tool_use_id: None,
            tool_name: "ask".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: runtime::ToolPauseKind::UserInput(runtime::UserInputPreview {
                questions: vec![runtime::UserInputQuestion {
                    id: "q".to_string(),
                    header: "Choice".to_string(),
                    question: "Proceed?".to_string(),
                    options: vec![runtime::UserInputOption {
                        label: "Yes".to_string(),
                        description: "Continue".to_string(),
                    }],
                }],
            }),
        },
    ];

    for request in requests {
        let expected = serde_json::to_value(&request).unwrap();
        let protocol: wire::ToolPauseRequest = request.clone().into();
        assert_eq!(serde_json::to_value(&protocol).unwrap(), expected);
        assert_eq!(runtime::ToolPauseRequest::from(protocol), request);
    }
}

#[test]
fn plan_action_conversion_preserves_tagged_wire() {
    for action in [
        runtime::PlanApprovalAction::Approve {
            profile: runtime::PlanExecutionProfile::Main,
        },
        runtime::PlanApprovalAction::ApproveInNewThread {
            profile: runtime::PlanExecutionProfile::Auto,
        },
        runtime::PlanApprovalAction::ContinueDiscussing,
    ] {
        let expected = serde_json::to_value(action).unwrap();
        let protocol: wire::PlanApprovalAction = action.into();
        assert_eq!(serde_json::to_value(protocol).unwrap(), expected);
    }
}
