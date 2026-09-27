pub mod catalog;
pub mod input;
use omini_protocol as protocol;
use std::net::SocketAddr;
use std::path::PathBuf;

const CLIENT_ID_HEADER: &str = "x-omini-client-id";

// TUI 内部意图先收敛成 ClientRequest，再由这一层翻译成 HTTP/WS 协议调用。
#[derive(Debug, Clone)]
pub enum ClientRequest {
    RunSubmitUserInput {
        input: crate::client::input::ClientUserInput,
        client_echo_id: Option<String>,
    },
    RunInterveneInput {
        input: crate::client::input::ClientUserInput,
        client_echo_id: Option<String>,
    },
    AgentTaskSubmitInput {
        task_id: String,
        input: crate::client::input::ClientUserInput,
        client_echo_id: Option<String>,
    },
    AgentTaskCancel {
        task_id: String,
    },
    RunExecuteCommand {
        command: protocol::RunCommand,
        input: crate::client::input::ClientUserInput,
        client_echo_id: Option<String>,
    },
    RunCancel,
    ProfileToggle,
    ProfileSet {
        profile: protocol::ActiveProfile,
    },
    OpenModelPicker,
    ModelSelect {
        provider: String,
        model: String,
        thinking_effort: Option<protocol::ThinkingEffort>,
    },
    ModelThinkingEffortSet {
        effort: protocol::ThinkingEffort,
    },
    OpenThreadPicker,
    ThreadOpen {
        thread_id: String,
    },
    ThreadNew {
        profile: protocol::ActiveProfile,
    },
    ThreadRename {
        title: String,
    },
    ContextCompact {
        instructions: Option<String>,
    },
    ToolPauseResolve {
        tool_use_id: String,
        response: protocol::ToolPauseResponse,
    },
    PlanResolve {
        action: protocol::PlanApprovalAction,
    },
    OpenAgentManager,
    AgentSave {
        source_kind: protocol::AgentSourceKind,
        original_path: Option<PathBuf>,
        draft: protocol::AgentDraft,
    },
    AgentDelete {
        path: PathBuf,
    },
    AgentGenerate {
        source_kind: protocol::AgentSourceKind,
        description: String,
        tools: Vec<String>,
        disallow_tools: Vec<String>,
        agent_model: Option<String>,
        provider: String,
        model: String,
        thinking_effort: Option<protocol::ThinkingEffort>,
    },
    AppShutdown,
}

#[derive(Debug)]
pub struct ProjectConnection {
    pub addr: SocketAddr,
    pub project_id: String,
    // client_id 同时用于 HTTP header 和 WebSocket header，server 用它判断 controller 权限。
    pub client_id: String,
    pub open: protocol::OpenProjectResponse,
}

#[derive(Debug)]
pub struct ConfigurationConnection {
    pub addr: SocketAddr,
    pub project_id: String,
    pub client_id: String,
    pub status: protocol::ProjectConfigurationResponse,
}

#[derive(Debug)]
pub enum StartupConnection {
    Project(Box<ProjectConnection>),
    Configuration(ConfigurationConnection),
}

pub mod connection;
pub use connection::*;
pub mod requests;
pub use requests::*;
pub mod stream;
pub use stream::*;
pub mod events;
pub use events::*;
pub mod discovery;
pub use discovery::*;
pub mod local;
pub use local::*;
pub mod http;
pub use http::*;
pub mod mapping;
pub use mapping::*;
pub mod attachments;
pub use attachments::*;
#[cfg(test)]
mod tests {

    use super::*;
    use crate::app::event::RuntimeToUiEvent;
    use crate::app::event::ThreadUsageSnapshot;
    use chrono::Utc;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn agent_urls_use_only_project_scope() {
        let now = Utc::now();
        let connection = ProjectConnection {
            addr: "127.0.0.1:4317".parse().expect("address should parse"),
            project_id: "project-1".to_string(),
            client_id: "client-1".to_string(),
            open: protocol::OpenProjectResponse {
                project: protocol::ProjectSummary {
                    id: "project-1".to_string(),
                    name: "Project".to_string(),
                    path: "/repo".to_string(),
                    storage_key: "project-1".to_string(),
                    path_status: protocol::ProjectPathStatus::Ready,
                    created_at: now,
                    updated_at: now,
                    last_opened_at: Some(now),
                },
                threads: Vec::new(),
                active_provider: "openai".to_string(),
                model: "test".to_string(),
                thinking_effort: None,
                context_window: None,
                mcp_server_count: 0,
                has_project_instructions: false,
                agents: Vec::new(),
                skills: Vec::new(),
                git_branch: None,
            },
        };

        let urls = [
            project_agents_url(&connection),
            project_agents_url_with_target(&connection, Some("thread/1")),
            project_agent_generate_url(&connection),
            project_agent_url(&connection, "/tmp/agent.md", Some("thread/1")),
        ];
        assert_eq!(
            urls,
            [
                "http://127.0.0.1:4317/v1/projects/project-1/agents",
                "http://127.0.0.1:4317/v1/projects/project-1/agents?target_thread_id=thread%2F1",
                "http://127.0.0.1:4317/v1/projects/project-1/agents/generate",
                "http://127.0.0.1:4317/v1/projects/project-1/agents/%2Ftmp%2Fagent.md?target_thread_id=thread%2F1",
            ]
        );
        assert!(urls.iter().all(|url| !url.contains("/threads/")));
    }

    fn query_runtime_status() -> protocol::ThreadRuntimeStatus {
        protocol::ThreadRuntimeStatus {
            thread_id: "thread_1".to_string(),
            state: protocol::ThreadRuntimeState::Working,
            active_profile: protocol::ActiveProfile::Main,
            loaded: true,
            controller_id: Some("client_1".to_string()),
            connected_client_count: 1,
            activity: Some(protocol::ThreadRuntimeActivity {
                kind: protocol::ThreadRuntimeActivityKind::Query,
                started_at: Utc::now(),
                elapsed_ms: 1_500,
            }),
            pending_pauses: Vec::new(),
            pending_plan_approval: None,
            active_tools: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            subagent_threads: Vec::new(),
            git_branch: None,
        }
    }

    fn runtime_event_envelope_text(event: protocol::TypedRuntimeEvent) -> String {
        serde_json::to_string(&protocol::ServerEnvelope::Event {
            event: protocol::RuntimeEvent::new(event),
        })
        .expect("envelope should serialize")
    }

    fn runtime_status_envelope_text(status: protocol::ThreadRuntimeStatus) -> String {
        serde_json::to_string(&protocol::ServerEnvelope::RuntimeStatus { status })
            .expect("envelope should serialize")
    }

    fn pending_pause() -> protocol::ToolPauseRequest {
        omini_runtime_contract::thread_domain::ToolPauseRequest {
            tool_use_id: "tool_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: omini_runtime_contract::thread_domain::ToolPauseKind::Permission(
                omini_runtime_contract::thread_domain::PermissionPreview::Custom {
                    tool_name: "bash".to_string(),
                    payload: serde_json::Map::new(),
                },
            ),
        }
        .into()
    }

    #[test]
    fn thread_summary_mapping_preserves_runtime_state() {
        let now = Utc::now();
        let summary = thread_summary_from_protocol(protocol::ThreadSummary {
            id: "thread_1".to_string(),
            title: "hello".to_string(),
            model: "gpt-test".to_string(),
            provider: "openai".to_string(),
            created_at: now,
            updated_at: now,
            runtime_state: Some(protocol::ThreadRuntimeState::Compacting),
        });

        assert_eq!(
            summary.runtime_state,
            Some(omini_runtime_contract::thread_domain::ThreadRuntimeState::Compacting)
        );
    }

    #[test]
    fn generated_agent_draft_preserves_local_policy_fields() {
        let draft = generated_agent_draft_from_protocol(
            protocol::GeneratedAgentDraft {
                name: "diff-reviewer".to_string(),
                description: "Use when reviewing diffs.".to_string(),
                short_description: None,
                instructions: "Review the diff and report findings.".to_string(),
            },
            vec!["read".to_string()],
            vec!["bash".to_string()],
            Some("openai/reasoner".to_string()),
        );

        assert_eq!(draft.name, "diff-reviewer");
        assert_eq!(draft.description, "Use when reviewing diffs.");
        assert_eq!(draft.instructions, "Review the diff and report findings.");
        assert_eq!(draft.tools, vec!["read"]);
        assert_eq!(draft.disallow_tools, vec!["bash"]);
        assert_eq!(draft.model.as_deref(), Some("openai/reasoner"));
    }

    #[test]
    fn runtime_status_latency_adjusts_running_query() {
        let status =
            apply_runtime_status_latency(query_runtime_status(), Duration::from_millis(42));

        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(1_542)
        );
    }

    #[test]
    fn runtime_status_latency_adjusts_compact_activity() {
        let mut status = query_runtime_status();
        status.state = protocol::ThreadRuntimeState::Compacting;
        status.activity = Some(protocol::ThreadRuntimeActivity {
            kind: protocol::ThreadRuntimeActivityKind::Compact,
            started_at: Utc::now(),
            elapsed_ms: 700,
        });

        let status = apply_runtime_status_latency(status, Duration::from_millis(88));

        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(788)
        );
    }

    #[test]
    fn runtime_status_latency_skips_waiting_or_pending_pause() {
        let mut waiting = query_runtime_status();
        waiting.state = protocol::ThreadRuntimeState::Waiting;
        let waiting = apply_runtime_status_latency(waiting, Duration::from_millis(42));
        assert_eq!(
            waiting
                .activity
                .as_ref()
                .map(|activity| activity.elapsed_ms),
            Some(1_500)
        );

        let mut pending = query_runtime_status();
        pending.pending_pauses.push(pending_pause());
        let pending = apply_runtime_status_latency(pending, Duration::from_millis(42));
        assert_eq!(
            pending
                .activity
                .as_ref()
                .map(|activity| activity.elapsed_ms),
            Some(1_500)
        );
    }

    #[test]
    fn runtime_status_latency_saturates_elapsed_ms() {
        let mut status = query_runtime_status();
        status
            .activity
            .as_mut()
            .expect("query status has activity")
            .elapsed_ms = u64::MAX - 5;

        let status = apply_runtime_status_latency(status, Duration::from_millis(10));

        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(u64::MAX)
        );
    }

    #[tokio::test]
    async fn handle_server_text_emits_runtime_status_sync() {
        let (tx, mut rx) = mpsc::channel(4);

        let outcome =
            handle_server_text(&runtime_status_envelope_text(query_runtime_status()), &tx)
                .await
                .expect("runtime status should decode");

        assert_eq!(outcome, HandleOutcome::SawRuntimeStatus);
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeToUiEvent::RuntimeStatusSynced {
                status,
                restore_pending_pauses: true,
            }) if status.thread_id == "thread_1" && status.activity.is_some()
        ));
    }

    #[tokio::test]
    async fn handle_server_text_decodes_thread_snapshot_event() {
        let (tx, mut rx) = mpsc::channel(4);

        let outcome = handle_server_text(
            &runtime_event_envelope_text(protocol::TypedRuntimeEvent::ThreadSnapshot(
                protocol::ThreadSnapshotEvent {
                    thread_id: "thread_1".to_string(),
                    messages: Vec::new(),
                    agent_tasks: Vec::new(),
                    usage: ThreadUsageSnapshot::default(),
                },
            )),
            &tx,
        )
        .await
        .expect("thread changed should decode");

        assert_eq!(outcome, HandleOutcome::PassThrough);
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeToUiEvent::ThreadSnapshot { .. })
        ));
    }

    #[tokio::test]
    async fn handle_server_text_emits_thread_switched() {
        let (tx, mut rx) = mpsc::channel(4);

        // ThreadSwitched 现在嵌入在 RuntimeEvent 中(由 server 通过普通runtime 通道广播),不是独立的 ServerEnvelope 变体。
        let envelope = serde_json::to_string(&protocol::ServerEnvelope::Event {
            event: protocol::RuntimeEvent::new(protocol::TypedRuntimeEvent::ThreadSwitched(
                protocol::ThreadSwitchedEvent {
                    from: "thread_old".to_string(),
                    to: "thread_new".to_string(),
                },
            )),
        })
        .expect("thread switched envelope should serialize");
        let outcome = handle_server_text(&envelope, &tx)
            .await
            .expect("thread switched should decode");

        // 主循环据此返回 ThreadLoop::Switch,触发 ws 重连到新 thread。
        assert_eq!(outcome, HandleOutcome::Switch("thread_new".to_string()));
        // ThreadSwitched 不应混入 runtime 事件流。
        assert!(rx.try_recv().is_err());
    }
}
pub mod configuration;
