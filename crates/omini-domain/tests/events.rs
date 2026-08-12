use chrono::{DateTime, TimeZone, Utc};
use omini_domain::events::{
    ActiveProfile, AgentTaskEvent, AgentTaskEventEnvelope, AgentTaskExecutionMode, AgentTaskInfo,
    AgentTaskResult, AgentTaskSnapshot, AgentTaskStatus, BashPermissionPreview, CompactTrigger,
    EditPermissionPreview, LoadedThread, McpPermissionPreview, Notification, NotificationKind,
    PermissionPreview, PlanApprovalAction, PlanExecutionProfile, ReadPermissionPreview,
    SearchPermissionPreview, ThreadRuntimeState, ThreadSummary, ThreadUsage, ThreadUsageSnapshot,
    ToolPauseKind, ToolPauseResponse, UserInputOption, UserInputPreview, UserInputQuestion,
};
use omini_domain::message::Message;
use serde_json::{Value, json};

#[test]
fn active_profile_all_variants_share_default_display_and_json_contracts() {
    assert_eq!(ActiveProfile::default(), ActiveProfile::Main);
    for (profile, text) in [
        (ActiveProfile::Main, "main"),
        (ActiveProfile::Auto, "auto"),
        (ActiveProfile::Plan, "plan"),
    ] {
        assert_eq!(profile.as_str(), text);
        assert_eq!(profile.to_string(), text);
        assert_eq!(
            serde_json::to_value(profile).expect("profile should serialize"),
            json!(text)
        );
        assert_eq!(
            serde_json::from_value::<ActiveProfile>(json!(text))
                .expect("profile should deserialize"),
            profile
        );
    }
}

#[test]
fn task_modes_statuses_and_compact_triggers_are_exhaustive() {
    for (mode, text) in [
        (AgentTaskExecutionMode::Background, "background"),
        (AgentTaskExecutionMode::Synchronous, "synchronous"),
    ] {
        assert_eq!(mode.as_str(), text);
        assert_eq!(
            serde_json::to_value(mode).expect("task mode should serialize"),
            json!(text)
        );
    }

    for (status, text, terminal) in [
        (AgentTaskStatus::Running, "running", false),
        (AgentTaskStatus::Cancelling, "cancelling", false),
        (AgentTaskStatus::Completed, "completed", true),
        (AgentTaskStatus::Failed, "failed", true),
        (AgentTaskStatus::Cancelled, "cancelled", true),
        (AgentTaskStatus::Interrupted, "interrupted", true),
    ] {
        assert_eq!(status.as_str(), text);
        assert_eq!(status.is_terminal(), terminal);
        assert_eq!(
            serde_json::to_value(status).expect("task status should serialize"),
            json!(text)
        );
    }

    for (trigger, text) in [
        (CompactTrigger::Auto, "auto"),
        (CompactTrigger::Manual, "manual"),
    ] {
        assert_eq!(trigger.as_str(), text);
        assert_eq!(trigger.to_string(), text);
        assert_eq!(
            serde_json::to_value(trigger).expect("compact trigger should serialize"),
            json!(text)
        );
    }
}

#[test]
fn plan_profiles_and_actions_preserve_execution_choice() {
    assert_eq!(
        PlanExecutionProfile::Main.active_profile(),
        ActiveProfile::Main
    );
    assert_eq!(
        PlanExecutionProfile::Auto.active_profile(),
        ActiveProfile::Auto
    );

    for (action, value) in [
        (
            PlanApprovalAction::Approve {
                profile: PlanExecutionProfile::Main,
            },
            json!({"type": "approve", "profile": "main"}),
        ),
        (
            PlanApprovalAction::ApproveInNewThread {
                profile: PlanExecutionProfile::Auto,
            },
            json!({"type": "approve_in_new_thread", "profile": "auto"}),
        ),
        (
            PlanApprovalAction::ContinueDiscussing,
            json!({"type": "continue_discussing"}),
        ),
    ] {
        assert_eq!(
            serde_json::to_value(action).expect("plan action should serialize"),
            value
        );
    }
}

#[test]
fn notification_constructors_set_kind_message_and_details() {
    for (notification, kind) in [
        (Notification::info("info"), NotificationKind::Info),
        (Notification::warning("warn"), NotificationKind::Warn),
        (Notification::error("error"), NotificationKind::Error),
    ] {
        assert_eq!(notification.kind, kind);
        assert!(notification.details.is_empty());
    }

    assert_eq!(
        Notification::warning("problem").with_details(vec!["first".into(), "second".into()]),
        Notification {
            kind: NotificationKind::Warn,
            message: "problem".into(),
            details: vec!["first".into(), "second".into()],
        }
    );
}

#[test]
fn thread_runtime_state_all_variants_use_snake_case_json_names() {
    assert_eq!(ThreadRuntimeState::default(), ThreadRuntimeState::Idle);
    for (state, text) in [
        (ThreadRuntimeState::Idle, "idle"),
        (ThreadRuntimeState::Working, "working"),
        (ThreadRuntimeState::Thinking, "thinking"),
        (ThreadRuntimeState::Waiting, "waiting"),
        (ThreadRuntimeState::Compacting, "compacting"),
    ] {
        assert_eq!(
            serde_json::to_value(state).expect("runtime state should serialize"),
            json!(text)
        );
    }
}

#[test]
fn thread_summary_omits_absent_runtime_state_and_preserves_present_state() {
    let input = json!({
        "id": "thread-1",
        "title": "Thread",
        "model": "model",
        "provider": "provider",
        "created_at": "2026-08-12T00:00:00Z",
        "updated_at": "2026-08-12T01:00:00Z"
    });
    let summary: ThreadSummary =
        serde_json::from_value(input.clone()).expect("thread summary should deserialize");
    assert_eq!(summary.runtime_state, None);
    assert_eq!(
        serde_json::to_value(summary).expect("thread summary should serialize"),
        input
    );

    let loaded = ThreadSummary {
        runtime_state: Some(ThreadRuntimeState::Working),
        ..thread_summary()
    };
    assert_eq!(
        serde_json::to_value(loaded).expect("loaded summary should serialize")["runtime_state"],
        json!("working")
    );
}

#[test]
fn loaded_thread_missing_profile_defaults_to_main() {
    let loaded: LoadedThread = serde_json::from_value(json!({
        "thread_id": "thread-1",
        "provider": "provider",
        "model": "model",
        "thinking_effort": null,
        "title": null,
        "messages": [],
        "agent_tasks": [],
        "usage": {
            "current_context_tokens": 0,
            "total_tokens": 0,
            "total_cached_tokens": 0,
            "context_window": null
        }
    }))
    .expect("loaded thread should deserialize");

    assert_eq!(loaded.active_profile, ActiveProfile::Main);
}

#[test]
fn usage_snapshot_conversion_preserves_counts_and_controls_optional_window() {
    let without_window = ThreadUsage::from(ThreadUsageSnapshot {
        current_context_tokens: 1,
        total_tokens: 2,
        total_cached_tokens: 3,
        context_window: None,
    });
    assert_eq!(
        serde_json::to_value(without_window).expect("usage should serialize"),
        json!({
            "current_context_tokens": 1,
            "total_tokens": 2,
            "total_cached_tokens": 3
        })
    );

    let with_window = ThreadUsage::from(ThreadUsageSnapshot {
        current_context_tokens: -1,
        total_tokens: i64::MAX,
        total_cached_tokens: i64::MIN,
        context_window: Some(u32::MAX),
    });
    assert_eq!(with_window.current_context_tokens, -1);
    assert_eq!(with_window.total_tokens, i64::MAX);
    assert_eq!(with_window.total_cached_tokens, i64::MIN);
    assert_eq!(with_window.context_window, Some(u32::MAX));
    assert_eq!(
        serde_json::to_value(with_window).expect("usage should serialize")["context_window"],
        json!(u32::MAX)
    );
}

#[test]
fn agent_task_optional_fields_default_and_omit_as_declared() {
    let input = json!({
        "task_id": "task-1",
        "thread_id": "child-1",
        "owner_thread_id": "owner-1",
        "parent_thread_id": "parent-1",
        "spawn_tool_use_id": "tool-1",
        "agent": "reviewer",
        "title": "Review",
        "depth": 1,
        "execution_mode": "background",
        "status": "running",
        "created_at": "2026-08-12T00:00:00Z",
        "updated_at": "2026-08-12T00:00:00Z"
    });
    let task: AgentTaskInfo =
        serde_json::from_value(input).expect("minimal task should deserialize");
    assert_eq!(task.parent_task_id, None);
    assert_eq!(task.result, None);
    assert_eq!(task.completed_at, None);
    assert!(!task.notification_delivered);

    let value = serde_json::to_value(task).expect("task should serialize");
    assert_eq!(value.get("parent_task_id"), None);
    assert_eq!(value.get("result"), None);
    assert_eq!(value.get("completed_at"), None);
    assert_eq!(value["notification_delivered"], json!(false));

    let empty_result: AgentTaskResult =
        serde_json::from_value(json!({})).expect("empty task result should deserialize");
    assert_eq!(
        serde_json::to_value(empty_result).expect("empty result should serialize"),
        json!({})
    );
}

#[test]
fn agent_task_snapshot_flattens_task_fields_and_keeps_messages() {
    let snapshot = AgentTaskSnapshot {
        task: task_info(),
        messages: vec![Message::from_user_text("hello".into())],
    };

    let value = serde_json::to_value(snapshot).expect("snapshot should serialize");
    assert_eq!(value.get("task"), None);
    assert_eq!(value["task_id"], json!("task-1"));
    assert_eq!(
        value["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "hello"}]}])
    );
}

#[test]
fn agent_task_event_defaults_are_explicit_at_the_boundary() {
    let envelope = AgentTaskEventEnvelope {
        task_id: "task-1".into(),
        thread_id: "child-1".into(),
        parent_task_id: None,
        owner_thread_id: "owner-1".into(),
        truncated: false,
        payload: AgentTaskEvent::TurnStarted,
    };
    let value = serde_json::to_value(envelope).expect("envelope should serialize");
    assert_eq!(value.get("parent_task_id"), None);
    assert_eq!(value.get("truncated"), None);
    assert_eq!(value["payload"], json!({"type": "turn_started"}));

    let event: AgentTaskEvent = serde_json::from_value(json!({
        "type": "message_committed",
        "message": {"role": "assistant", "content": [{"type": "text", "text": "done"}]}
    }))
    .expect("message event should deserialize");
    assert!(matches!(
        &event,
        AgentTaskEvent::MessageCommitted {
            persist_llm_history: false,
            ..
        }
    ));
    assert_eq!(
        serde_json::to_value(event).expect("message event should serialize")["persist_llm_history"],
        json!(false)
    );
}

#[test]
fn permission_preview_all_variants_use_distinct_tags() {
    let cases = [
        (
            PermissionPreview::Bash(BashPermissionPreview {
                command: "cargo test".into(),
                description: None,
                workdir: Some("/workspace".into()),
                timeout: 30,
            }),
            json!({
                "type": "bash",
                "command": "cargo test",
                "description": null,
                "workdir": "/workspace",
                "timeout": 30
            }),
        ),
        (
            PermissionPreview::Edit(edit_preview()),
            edit_preview_json("edit"),
        ),
        (
            PermissionPreview::Write(edit_preview()),
            edit_preview_json("write"),
        ),
        (
            PermissionPreview::Read(ReadPermissionPreview {
                file_path: "/workspace/src/lib.rs".into(),
            }),
            json!({"type": "read", "file_path": "/workspace/src/lib.rs"}),
        ),
        (
            PermissionPreview::Search(SearchPermissionPreview {
                query: "needle".into(),
                mode: "content".into(),
                path: "/workspace".into(),
            }),
            json!({"type": "search", "query": "needle", "mode": "content", "path": "/workspace"}),
        ),
        (
            PermissionPreview::Mcp(McpPermissionPreview {
                server_name: "server".into(),
                server_tool_name: "remote".into(),
                registered_tool_name: "server_remote".into(),
                inputs: json!({"key": [1, true]})
                    .as_object()
                    .expect("fixture should be an object")
                    .clone(),
            }),
            json!({
                "type": "mcp",
                "server_name": "server",
                "server_tool_name": "remote",
                "registered_tool_name": "server_remote",
                "inputs": {"key": [1, true]}
            }),
        ),
        (
            PermissionPreview::Custom {
                tool_name: "custom".into(),
                payload: json!({"nested": {"value": 1}})
                    .as_object()
                    .expect("fixture should be an object")
                    .clone(),
            },
            json!({"type": "custom", "tool_name": "custom", "payload": {"nested": {"value": 1}}}),
        ),
    ];

    for (preview, value) in cases {
        assert_eq!(
            serde_json::to_value(&preview).expect("preview should serialize"),
            value
        );
        assert_eq!(
            serde_json::from_value::<PermissionPreview>(value).expect("preview should deserialize"),
            preview
        );
    }
}

#[test]
fn tool_pause_permission_and_user_input_round_trip_their_custom_shapes() {
    let permission = ToolPauseKind::Permission(PermissionPreview::Write(edit_preview()));
    let permission_value = json!({
        "type": "permission",
        "preview": {
            "type": "write",
            "summary": "Create file",
            "path": "/tmp/new.txt",
            "replacement_count": 0,
            "diff": ""
        }
    });
    assert_round_trip_pause(permission, permission_value);

    let user_input = ToolPauseKind::UserInput(UserInputPreview {
        questions: vec![UserInputQuestion {
            id: "choice".into(),
            header: "Mode".into(),
            question: "Choose a mode".into(),
            options: vec![UserInputOption {
                label: "Safe".into(),
                description: "Use safe mode".into(),
            }],
        }],
    });
    let user_input_value = json!({
        "type": "user_input",
        "questions": [{
            "id": "choice",
            "header": "Mode",
            "question": "Choose a mode",
            "options": [{"label": "Safe", "description": "Use safe mode"}]
        }]
    });
    assert_round_trip_pause(user_input, user_input_value);
}

#[test]
fn malformed_tool_pause_shapes_are_rejected_with_the_relevant_reason() {
    for (value, reason) in [
        (json!({}), "missing tool pause kind type"),
        (json!({"type": "unknown"}), "unknown tool pause kind type"),
        (
            json!({"type": "permission"}),
            "permission tool pause missing preview",
        ),
        (
            json!({"type": "permission", "preview": {"type": "read"}}),
            "file_path",
        ),
        (json!({"type": "user_input"}), "questions"),
        (
            json!({
                "type": "write",
                "summary": "Create file",
                "path": "/tmp/new.txt",
                "replacement_count": 0,
                "diff": ""
            }),
            "unknown tool pause kind type 'write'",
        ),
    ] {
        let error = serde_json::from_value::<ToolPauseKind>(value)
            .expect_err("malformed pause should be rejected");
        assert!(
            error.to_string().contains(reason),
            "expected {reason:?} in {error}"
        );
    }
}

#[test]
fn tool_pause_responses_cover_approval_input_and_cancellation_shapes() {
    let cases = [
        (
            ToolPauseResponse::Permission {
                approved: true,
                note: Some("once".into()),
            },
            json!({"type": "permission", "approved": true, "note": "once"}),
        ),
        (
            ToolPauseResponse::Permission {
                approved: false,
                note: None,
            },
            json!({"type": "permission", "approved": false}),
        ),
        (
            ToolPauseResponse::UserInput {
                value: json!({"choice": "Safe", "note": null}),
            },
            json!({"type": "user_input", "value": {"choice": "Safe", "note": null}}),
        ),
        (ToolPauseResponse::Cancelled, json!({"type": "cancelled"})),
    ];

    for (response, value) in cases {
        assert_eq!(
            serde_json::to_value(&response).expect("pause response should serialize"),
            value
        );
        assert_eq!(
            serde_json::from_value::<ToolPauseResponse>(value)
                .expect("pause response should deserialize"),
            response
        );
    }
}

fn assert_round_trip_pause(pause: ToolPauseKind, value: Value) {
    assert_eq!(
        serde_json::to_value(&pause).expect("tool pause should serialize"),
        value
    );
    assert_eq!(
        serde_json::from_value::<ToolPauseKind>(value).expect("tool pause should deserialize"),
        pause
    );
}

fn edit_preview() -> EditPermissionPreview {
    EditPermissionPreview {
        summary: "Create file".into(),
        path: "/tmp/new.txt".into(),
        replacement_count: 0,
        diff: String::new(),
    }
}

fn edit_preview_json(kind: &str) -> Value {
    json!({
        "type": kind,
        "summary": "Create file",
        "path": "/tmp/new.txt",
        "replacement_count": 0,
        "diff": ""
    })
}

fn task_info() -> AgentTaskInfo {
    AgentTaskInfo {
        task_id: "task-1".into(),
        thread_id: "child-1".into(),
        parent_task_id: None,
        owner_thread_id: "owner-1".into(),
        parent_thread_id: "parent-1".into(),
        spawn_tool_use_id: "tool-1".into(),
        agent: "reviewer".into(),
        title: "Review".into(),
        depth: 1,
        execution_mode: AgentTaskExecutionMode::Background,
        status: AgentTaskStatus::Running,
        result: None,
        created_at: fixed_time(),
        updated_at: fixed_time(),
        completed_at: None,
        notification_delivered: false,
    }
}

fn thread_summary() -> ThreadSummary {
    ThreadSummary {
        id: "thread-1".into(),
        title: "Thread".into(),
        model: "model".into(),
        provider: "provider".into(),
        created_at: fixed_time(),
        updated_at: fixed_time(),
        runtime_state: None,
    }
}

fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 12, 0, 0, 0)
        .single()
        .expect("fixed test time should be valid")
}
