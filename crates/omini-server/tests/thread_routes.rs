mod support;

use futures_util::StreamExt;
use omini_protocol::{
    AckResponse, AttachmentUploadResponse, ClientThreadRole, CreateProjectRequest,
    CreateThreadRequest, ProtocolError, RegisterClientRequest, RegisterClientResponse,
    RenameThreadRequest, RunInputMode, ServerEnvelope, SubmitRunRequest, ThreadStatusesResponse,
    ThreadsResponse, TypedRuntimeEvent, UserInput,
};
use reqwest::Method;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

async fn project_and_thread(daemon: &support::TestDaemon) -> (String, String) {
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

    let (status, thread): (_, omini_protocol::CreateThreadResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{}/threads", project.id),
            None,
            &CreateThreadRequest::default(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    (project.id, thread.thread_id)
}

async fn register_client(daemon: &support::TestDaemon) -> String {
    let (status, response): (_, RegisterClientResponse) = daemon
        .send_json(
            Method::POST,
            "/clients",
            None,
            &RegisterClientRequest {
                kind: Some("test".to_string()),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    uuid::Uuid::parse_str(&response.client_id).expect("client ID should be a UUID");
    response.client_id
}

#[tokio::test]
async fn threads_run_requires_connected_client() {
    let mut daemon = support::TestDaemon::start("thread-auth").await;
    let (project_id, thread_id) = project_and_thread(&daemon).await;
    let request = SubmitRunRequest {
        input: UserInput::plain("hello"),
        client_echo_id: None,
        mode: RunInputMode::Submit,
    };

    // 缺 header 和未连接的已注册客户端是两个对调用方有区别的拒绝状态。
    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/runs"),
            None,
            &request,
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(error.code, "missing_client_id");
    assert_eq!(
        error.message,
        "Mutating requests must include x-omini-client-id"
    );

    let client_id = register_client(&daemon).await;
    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/runs"),
            Some(&client_id),
            &request,
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(error.code, "client_not_connected");
    assert_eq!(
        error.message,
        "This client is not connected to the thread event stream"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn threads_websocket_initializes_in_protocol_order() {
    let mut daemon = support::TestDaemon::start("thread-websocket").await;
    let (project_id, thread_id) = project_and_thread(&daemon).await;
    let client_id = register_client(&daemon).await;
    let mut request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{thread_id}/events"
        ))
        .into_client_request()
        .expect("WebSocket request should build");
    request.headers_mut().insert(
        "x-omini-client-id",
        HeaderValue::from_str(&client_id).expect("client ID should be a valid header"),
    );

    let (mut socket, _) = connect_async(request)
        .await
        .expect("thread WebSocket should connect");
    let mut envelopes = Vec::new();
    for _ in 0..7 {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), socket.next())
            .await
            .expect("initial WebSocket envelope should arrive")
            .expect("WebSocket should remain open")
            .expect("initial WebSocket frame should be valid")
            .into_text()
            .expect("initial WebSocket frame should be text");
        envelopes.push(
            serde_json::from_str::<ServerEnvelope>(&message)
                .expect("initial WebSocket envelope should decode"),
        );
    }

    assert!(matches!(
        &envelopes[0],
        ServerEnvelope::ControllerChanged { controller_id }
            if controller_id.as_deref() == Some(client_id.as_str())
    ));
    assert!(matches!(
        &envelopes[1],
        ServerEnvelope::ClientRoleChanged {
            client_id: envelope_client_id,
            role: ClientThreadRole::Controller,
            controller_id: Some(controller_id),
        } if envelope_client_id == &client_id && controller_id == &client_id
    ));
    assert_eq!(
        envelopes[2..6]
            .iter()
            .map(|envelope| match envelope {
                ServerEnvelope::Event { event } => event.kind(),
                _ => panic!("snapshot initialization should contain runtime events"),
            })
            .collect::<Vec<_>>(),
        vec![
            "thread_title_changed",
            "model_changed",
            "active_profile_changed",
            "thread_snapshot",
        ]
    );
    assert!(matches!(
        &envelopes[5],
        ServerEnvelope::Event {
            event: omini_protocol::RuntimeEvent {
                event: TypedRuntimeEvent::ThreadSnapshot(snapshot),
            }
        } if snapshot.thread_id == thread_id && snapshot.messages.is_empty() && snapshot.agent_tasks.is_empty()
    ));
    assert!(matches!(
        &envelopes[6],
        ServerEnvelope::RuntimeStatus { status }
            if status.thread_id == thread_id
                && status.connected_client_count == 1
                && status.controller_id.as_deref() == Some(client_id.as_str())
    ));

    daemon.shutdown().await;
}

#[tokio::test]
async fn threads_unknown_skill_rejects() {
    let mut daemon = support::TestDaemon::start("thread-unknown-skill").await;
    let (project_id, thread_id) = project_and_thread(&daemon).await;

    let (status, error): (_, ProtocolError) = daemon
        .get(&format!(
            "/projects/{project_id}/threads/{thread_id}/skills/not-installed"
        ))
        .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(error.code, "skill_not_found");
    assert_eq!(error.message, "Skill 'not-installed' does not exist");

    daemon.shutdown().await;
}

#[tokio::test]
async fn threads_controller_mutations_preserve_contract() {
    let mut daemon = support::TestDaemon::start("thread-mutations").await;
    let (project_id, thread_id) = project_and_thread(&daemon).await;
    let client_id = register_client(&daemon).await;
    let mut request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{thread_id}/events"
        ))
        .into_client_request()
        .expect("WebSocket request should build");
    request.headers_mut().insert(
        "x-omini-client-id",
        HeaderValue::from_str(&client_id).expect("client ID should be a valid header"),
    );
    let (socket, _) = connect_async(request)
        .await
        .expect("thread WebSocket should connect");

    // 重命名要求已有 controller；这个连接同时覆盖 server 自动授予的初始控制权。
    let requested_title = format!("  {}  ", "界".repeat(400));
    let (status, response): (_, AckResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/rename"),
            Some(&client_id),
            &RenameThreadRequest {
                title: requested_title,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(response, AckResponse::ok());

    let (status, threads): (_, ThreadsResponse) =
        daemon.get(&format!("/projects/{project_id}/threads")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(threads.threads.len(), 1);
    assert_eq!(threads.threads[0].id, thread_id);
    assert_eq!(threads.threads[0].title, "界".repeat(300));

    let response = daemon
        .send_bytes(
            &format!("/projects/{project_id}/threads/{thread_id}/attachments"),
            Some(&client_id),
            "image/png",
            vec![0, 1, 2, 3],
        )
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .json::<AttachmentUploadResponse>()
            .await
            .expect("attachment response should decode"),
        AttachmentUploadResponse {
            attachment: omini_protocol::AttachmentMetadata {
                attachment_id: "054edec1d0211f624fed0cbca9d4f9400b0e491c43742af2c5b0abebf0c990d8"
                    .to_string(),
                mime_type: "image/png".to_string(),
                size: 4,
                name: "attachment".to_string(),
            },
        }
    );

    let (status, statuses): (_, ThreadStatusesResponse) = daemon
        .get(&format!(
            "/projects/{project_id}/threads/statuses?status=idle,%20working"
        ))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(statuses.statuses.len(), 1);
    assert_eq!(statuses.statuses[0].thread_id, thread_id);
    assert_eq!(
        statuses.statuses[0].state,
        omini_protocol::ThreadRuntimeState::Idle
    );

    let (status, error): (_, ProtocolError) = daemon
        .get(&format!(
            "/projects/{project_id}/threads/statuses?status=idle,busy"
        ))
        .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error.code, "invalid_status_filter");
    assert_eq!(error.message, "Invalid thread status filter: busy");

    drop(socket);
    daemon.shutdown().await;
}
