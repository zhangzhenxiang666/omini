mod support;

use futures_util::StreamExt;
use omini_protocol::{
    AckResponse, AgentDraft, AgentSourceKind, AgentsResponse, CreateProjectRequest,
    CreateThreadRequest, ProtocolError, SaveAgentRequest, ServerEnvelope, TypedRuntimeEvent,
};
use reqwest::Method;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

fn draft(name: &str) -> AgentDraft {
    AgentDraft {
        name: name.to_string(),
        description: format!("Use when {name} is needed."),
        short_description: Some(format!("{name} helper")),
        instructions: format!("Run the {name} workflow."),
        tools: vec!["read".to_string()],
        disallow_tools: vec!["write".to_string()],
        model: Some("openai/reasoner".to_string()),
    }
}

async fn project_id(daemon: &support::TestDaemon) -> String {
    let workspace = daemon.root().create_dir("workspace");
    let (status, project): (_, omini_protocol::ProjectSummary) = daemon
        .send_json(
            Method::POST,
            "/projects",
            None,
            &CreateProjectRequest {
                path: workspace.display().to_string(),
                name: None,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    project.id
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::test]
async fn agents_project_draft_round_trips() {
    let mut daemon = support::TestDaemon::start("agent-crud").await;
    let project_id = project_id(&daemon).await;
    let request = SaveAgentRequest {
        source_kind: AgentSourceKind::Project,
        original_agent_id: None,
        draft: draft("cache-helper"),
    };

    let (status, response): (_, AckResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/agents"),
            None,
            &request,
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(response, AckResponse::ok());

    let (status, agents): (_, AgentsResponse) =
        daemon.get(&format!("/projects/{project_id}/agents")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let saved = agents
        .records
        .iter()
        .find(|record| record.name == "cache-helper")
        .expect("saved project agent should be listed");
    assert_eq!(saved.description, "Use when cache-helper is needed.");
    assert_eq!(
        saved.short_description.as_deref(),
        Some("cache-helper helper")
    );
    assert_eq!(saved.instructions, "Run the cache-helper workflow.");
    assert_eq!(saved.tools, vec!["read"]);
    assert_eq!(saved.disallow_tools, vec!["write"]);
    assert_eq!(saved.model.as_deref(), Some("openai/reasoner"));
    assert_eq!(saved.source_kind, AgentSourceKind::Project);
    assert!(saved.editable);
    let agent_id = saved.id.clone();

    let response = daemon
        .client()
        .delete(daemon.url(&format!(
            "/projects/{project_id}/agents/{}",
            percent_encode(&agent_id)
        )))
        .send()
        .await
        .expect("delete request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .json::<AckResponse>()
            .await
            .expect("delete response should decode"),
        AckResponse::ok()
    );

    let (status, agents): (_, AgentsResponse) =
        daemon.get(&format!("/projects/{project_id}/agents")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(agents.records.iter().all(|record| record.id != agent_id));

    daemon.shutdown().await;
}

#[tokio::test]
async fn agents_uneditable_records_reject_writes() {
    let mut daemon = support::TestDaemon::start("agent-rejections").await;
    let project_id = project_id(&daemon).await;

    // 内置来源和不存在的记录都不可编辑，但前者是保存时拒绝，后者是删除时拒绝。
    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/agents"),
            None,
            &SaveAgentRequest {
                source_kind: AgentSourceKind::BuiltIn,
                original_agent_id: None,
                draft: draft("forbidden"),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error.code, "core_error");
    assert!(error.message.contains("内置 agent 不能写入"));

    let response = daemon
        .client()
        .delete(daemon.url(&format!("/projects/{project_id}/agents/missing")))
        .send()
        .await
        .expect("delete request should complete");
    let status = response.status();
    let error: ProtocolError = response
        .json()
        .await
        .expect("unknown agent error should decode");
    assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error.code, "core_error");
    assert!(error.message.contains("不存在或不可编辑"));

    daemon.shutdown().await;
}

#[tokio::test]
async fn agents_target_thread_broadcasts_update() {
    let mut daemon = support::TestDaemon::start("agent-target-update").await;
    let project_id = project_id(&daemon).await;
    let (status, thread): (_, omini_protocol::CreateThreadResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads"),
            None,
            &CreateThreadRequest::default(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);

    let client_id = "observer-client";
    let mut request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{}/events",
            thread.thread_id
        ))
        .into_client_request()
        .expect("WebSocket request should build");
    request
        .headers_mut()
        .insert("x-omini-client-id", HeaderValue::from_static(client_id));
    let (mut socket, _) = connect_async(request)
        .await
        .expect("thread WebSocket should connect");
    for _ in 0..7 {
        tokio::time::timeout(std::time::Duration::from_secs(1), socket.next())
            .await
            .expect("initial WebSocket envelope should arrive")
            .expect("WebSocket should remain open")
            .expect("initial WebSocket frame should be valid");
    }

    let (status, response): (_, AckResponse) = daemon
        .send_json(
            Method::POST,
            &format!(
                "/projects/{project_id}/agents?target_thread_id={}",
                thread.thread_id
            ),
            None,
            &SaveAgentRequest {
                source_kind: AgentSourceKind::Project,
                original_agent_id: None,
                draft: draft("target-helper"),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(response, AckResponse::ok());

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let frame = socket
                .next()
                .await
                .expect("WebSocket should remain open")
                .expect("WebSocket frame should be valid")
                .into_text()
                .expect("server envelope should be text");
            let ServerEnvelope::Event { event } = serde_json::from_str::<ServerEnvelope>(&frame)
                .expect("server envelope should decode")
            else {
                continue;
            };
            if let TypedRuntimeEvent::AgentManagementUpdated { records } = event.event {
                break records;
            }
        }
    })
    .await
    .expect("agent management update should arrive");
    let record = event
        .iter()
        .find(|record| record.name == "target-helper")
        .expect("broadcast should include the saved agent");
    assert_eq!(record.instructions, "Run the target-helper workflow.");
    assert_eq!(record.source_kind, AgentSourceKind::Project);
    assert!(record.editable);

    daemon.shutdown().await;
}
