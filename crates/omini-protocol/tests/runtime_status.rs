use chrono::{DateTime, Utc};
use omini_protocol::{
    ActiveProfile, AgentSummary, PlanSubmittedEvent, ServerEnvelope, SkillSourceKind,
    ThreadRuntimeActivity, ThreadRuntimeActivityKind, ThreadRuntimeCapabilityStatus,
    ThreadRuntimeMcpServer, ThreadRuntimeMcpStatus, ThreadRuntimeMcpTool, ThreadRuntimeSkill,
    ThreadRuntimeState, ThreadRuntimeStatus, ThreadRuntimeTool,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[test]
fn thread_runtime_status_active_snapshot_round_trips_with_ordered_capabilities() {
    let status = active_status();
    let wire = serde_json::to_value(&status).unwrap();

    assert_eq!(
        wire,
        json!({
            "thread_id": "thread_1",
            "state": "working",
            "active_profile": "plan",
            "loaded": true,
            "controller_id": "client_1",
            "connected_client_count": 2,
            "activity": {
                "kind": "query",
                "started_at": "2024-01-02T03:04:05Z",
                "elapsed_ms": 250
            },
            "pending_plan_approval": {
                "plan_id": "plan_1",
                "title": "Plan",
                "markdown": "# Plan"
            },
            "active_tools": [{
                "tool_use_id": "tool_1",
                "tool_name": "bash",
                "started_at": "2024-01-02T03:04:05Z",
                "elapsed_ms": 100,
                "source_thread_id": "child_1",
                "source_agent_label": "explorer"
            }],
            "skills": [{
                "name": "review",
                "description": "Review a diff",
                "short_description": "Review",
                "source_kind": "project",
                "directory": "/work/.agents/review",
                "status": "available",
                "disable_model_invocation": false,
                "user_invocable": true
            }],
            "mcp_servers": [{
                "name": "search",
                "status": "ready",
                "tools": [{
                    "name": "query",
                    "registered_name": "search__query",
                    "description": "Search documents"
                }]
            }, {
                "name": "database",
                "status": "failed",
                "last_error": "connection refused"
            }],
            "subagent_threads": [{
                "name": "explorer",
                "description": "Explore files",
                "location": "project"
            }],
            "git_branch": "main"
        })
    );
    assert_eq!(
        serde_json::from_value::<ThreadRuntimeStatus>(wire).unwrap(),
        status
    );
}

#[test]
fn runtime_status_envelope_preserves_full_status_snapshot() {
    let status = active_status();
    let envelope = ServerEnvelope::RuntimeStatus {
        status: status.clone(),
    };
    let wire = serde_json::to_value(envelope).unwrap();

    assert_eq!(wire["type"], json!("runtime_status"));
    assert_eq!(wire["status"], serde_json::to_value(&status).unwrap());
    let decoded: ServerEnvelope = serde_json::from_value(wire).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        json!({ "type": "runtime_status", "status": serde_json::to_value(status).unwrap() })
    );
}

#[test]
fn thread_runtime_status_idle_snapshot_round_trips_after_empty_fields_are_omitted() {
    let status = ThreadRuntimeStatus {
        thread_id: "thread_1".to_string(),
        state: ThreadRuntimeState::Idle,
        active_profile: ActiveProfile::Main,
        loaded: false,
        controller_id: None,
        connected_client_count: 0,
        activity: None,
        pending_pauses: Vec::new(),
        pending_plan_approval: None,
        active_tools: Vec::new(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        subagent_threads: Vec::new(),
        git_branch: None,
    };
    let wire = serde_json::to_value(&status).unwrap();

    assert_eq!(
        wire,
        json!({
            "thread_id": "thread_1",
            "state": "idle",
            "active_profile": "main",
            "loaded": false,
            "connected_client_count": 0
        })
    );
    assert_eq!(
        serde_json::from_value::<ThreadRuntimeStatus>(wire).unwrap(),
        status
    );
}

#[test]
fn thread_runtime_status_missing_active_profile_defaults_to_main() {
    // 新字段缺失时必须兼容旧 daemon 已持久化或已缓存的状态快照。
    let status: ThreadRuntimeStatus = serde_json::from_value(json!({
        "thread_id": "thread_1",
        "state": "idle",
        "loaded": true,
        "connected_client_count": 0
    }))
    .expect("older status payloads should retain the main profile");

    assert_eq!(status.active_profile, ActiveProfile::Main);
    assert!(status.pending_pauses.is_empty());
    assert!(status.active_tools.is_empty());
    assert!(status.skills.is_empty());
    assert!(status.mcp_servers.is_empty());
    assert!(status.subagent_threads.is_empty());
}

#[test]
fn thread_runtime_status_missing_thread_id_is_data_error() {
    assert_data_error::<ThreadRuntimeStatus>(
        json!({ "state": "idle", "loaded": true, "connected_client_count": 0 }),
        "missing field",
    );
}

#[test]
fn runtime_status_enums_use_stable_wire_names() {
    for (kind, expected) in [
        (ThreadRuntimeActivityKind::Query, "query"),
        (ThreadRuntimeActivityKind::Compact, "compact"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(expected));
    }
    for (source, expected) in [
        (SkillSourceKind::BuiltIn, "built_in"),
        (SkillSourceKind::Project, "project"),
        (SkillSourceKind::User, "user"),
    ] {
        assert_eq!(serde_json::to_value(source).unwrap(), json!(expected));
    }
    for (status, expected) in [
        (ThreadRuntimeMcpStatus::Disabled, "disabled"),
        (ThreadRuntimeMcpStatus::Connecting, "connecting"),
        (ThreadRuntimeMcpStatus::Ready, "ready"),
        (ThreadRuntimeMcpStatus::Failed, "failed"),
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), json!(expected));
    }
    assert_eq!(
        serde_json::to_value(ThreadRuntimeCapabilityStatus::Available).unwrap(),
        json!("available")
    );
}

#[test]
fn runtime_status_unknown_mcp_status_is_data_error() {
    assert_data_error::<ThreadRuntimeMcpStatus>(json!("degraded"), "unknown variant");
}

fn active_status() -> ThreadRuntimeStatus {
    ThreadRuntimeStatus {
        thread_id: "thread_1".to_string(),
        state: ThreadRuntimeState::Working,
        active_profile: ActiveProfile::Plan,
        loaded: true,
        controller_id: Some("client_1".to_string()),
        connected_client_count: 2,
        activity: Some(ThreadRuntimeActivity {
            kind: ThreadRuntimeActivityKind::Query,
            started_at: fixed_time(),
            elapsed_ms: 250,
        }),
        pending_pauses: Vec::new(),
        pending_plan_approval: Some(PlanSubmittedEvent {
            plan_id: "plan_1".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan".to_string(),
        }),
        active_tools: vec![ThreadRuntimeTool {
            tool_use_id: "tool_1".to_string(),
            tool_name: "bash".to_string(),
            started_at: fixed_time(),
            elapsed_ms: 100,
            source_thread_id: Some("child_1".to_string()),
            source_agent_label: Some("explorer".to_string()),
        }],
        skills: vec![ThreadRuntimeSkill {
            name: "review".to_string(),
            description: "Review a diff".to_string(),
            short_description: Some("Review".to_string()),
            source_kind: SkillSourceKind::Project,
            directory: "/work/.agents/review".to_string(),
            status: ThreadRuntimeCapabilityStatus::Available,
            disable_model_invocation: false,
            user_invocable: true,
        }],
        mcp_servers: vec![
            ThreadRuntimeMcpServer {
                name: "search".to_string(),
                status: ThreadRuntimeMcpStatus::Ready,
                last_error: None,
                tools: vec![ThreadRuntimeMcpTool {
                    name: "query".to_string(),
                    registered_name: "search__query".to_string(),
                    description: "Search documents".to_string(),
                }],
            },
            ThreadRuntimeMcpServer {
                name: "database".to_string(),
                status: ThreadRuntimeMcpStatus::Failed,
                last_error: Some("connection refused".to_string()),
                tools: Vec::new(),
            },
        ],
        subagent_threads: vec![AgentSummary {
            name: "explorer".to_string(),
            description: "Explore files".to_string(),
            short_description: None,
            location: "project".to_string(),
        }],
        git_branch: Some("main".to_string()),
    }
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
        .unwrap()
        .with_timezone(&Utc)
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
