mod support;
use support::store::{fixed_time, test_agent_task, test_agent_thread};

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use omini_domain::agent_run::{AgentRunSnapshot as DomainRun, AgentRunStatus as DomainRunStatus};
use omini_model::message::Message as ModelMessage;
use omini_protocol::{
    AttachmentUploadResponse, ClientThreadRole, ControllerLease, CreateProjectRequest,
    CreateThreadRequest, InputPart, ProtocolError, RegisterClientResponse, RenameThreadRequest,
    RunSubmittedResponse, ServerEnvelope, SubmitRunRequest, ThreadRuntimeStatus,
    ThreadStatusesResponse, ThreadsResponse, TypedRuntimeEvent, UserInput,
};
use omini_runtime_contract::persistence::ClientMessage;
use omini_server::store::Store;
use reqwest::Method;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

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
    assert_eq!(status, reqwest::StatusCode::CREATED);

    let (status, thread): (_, omini_protocol::CreateThreadResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{}/threads", project.id),
            None,
            &CreateThreadRequest::default(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    (project.id, thread.thread_id)
}

async fn register_client(daemon: &support::TestDaemon) -> String {
    let response = daemon
        .client()
        .post(daemon.url("/clients"))
        .send()
        .await
        .expect("registration request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let response: RegisterClientResponse = response
        .json()
        .await
        .expect("registration response should decode");
    uuid::Uuid::parse_str(&response.client_id).expect("client ID should be a UUID");
    response.client_id
}

async fn close_socket<S>(socket: &mut S)
where
    S: Sink<Message, Error = WebSocketError>
        + Stream<Item = Result<Message, WebSocketError>>
        + Unpin,
{
    socket
        .send(Message::Close(None))
        .await
        .expect("WebSocket close frame should send");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("WebSocket should finish closing");
}

/// 跳过连接初始化帧，读取下一条子任务输入入队事件。
async fn next_child_input<S>(socket: &mut S) -> String
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let frame = socket
                .next()
                .await
                .expect("WebSocket should remain open")
                .unwrap();
            let envelope: ServerEnvelope =
                serde_json::from_str(&frame.into_text().unwrap()).unwrap();
            if let ServerEnvelope::Event {
                event:
                    omini_protocol::RuntimeEvent {
                        event:
                            TypedRuntimeEvent::AgentTaskUserMessageQueued {
                                client_id: Some(client_id),
                                ..
                            },
                    },
            } = envelope
            {
                return client_id;
            }
        }
    })
    .await
    .expect("child input event should arrive")
}

/// 读取重连快照，以核对投递事件和持久化历史的合并结果。
async fn next_thread_snapshot<S>(socket: &mut S) -> omini_protocol::ThreadSnapshotEvent
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let frame = socket
                .next()
                .await
                .expect("WebSocket should remain open")
                .unwrap();
            let envelope: ServerEnvelope =
                serde_json::from_str(&frame.into_text().unwrap()).unwrap();
            if let ServerEnvelope::Event {
                event:
                    omini_protocol::RuntimeEvent {
                        event: TypedRuntimeEvent::ThreadSnapshot(snapshot),
                    },
            } = envelope
            {
                return snapshot;
            }
        }
    })
    .await
    .expect("thread snapshot should arrive")
}

#[tokio::test]
/// 验证子任务输入按主线程归属定位，同时跨客户端保留独立投递。
async fn child_input_ownership() {
    // 给定父线程与子 Run 分别落在不同的 thread_id，另有一个无权访问的线程。
    let mut daemon = support::TestDaemon::start("child-input-owner").await;
    let (project_id, thread_id) = project_and_thread(&daemon).await;
    let (status, other): (_, omini_protocol::CreateThreadResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads"),
            None,
            &CreateThreadRequest::default(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let db = Store::open(&daemon.root().path().join(".omini/omini.db"))
        .await
        .unwrap();
    db.create_agent_run(&DomainRun {
        id: "parent-run".into(),
        thread_id: thread_id.clone(),
        parent_run_id: None,
        status: DomainRunStatus::Running,
        created_at: fixed_time(),
        started_at: Some(fixed_time()),
        finished_at: None,
        total_tokens: 0,
        archived_at: None,
    })
    .await
    .unwrap();
    let mut task = test_agent_task("child-run", "child-thread", &thread_id);
    task.parent_run_id = Some("parent-run".into());
    db.create_agent_task(
        &project_id,
        &task,
        &test_agent_thread("child-thread", &thread_id),
        &ModelMessage::from_user_text("start".into()),
    )
    .await
    .unwrap();
    let client_id = register_client(&daemon).await;
    let mut request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{thread_id}/events"
        ))
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "x-omini-client-id",
        HeaderValue::from_str(&client_id).unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();

    // 当父线程投递时，路由能定位子线程的 Run；其他主线程不能复用该 task ID。
    let input = omini_protocol::AgentRunInputRequest {
        input: UserInput::plain("follow up"),
        client_echo_id: "echo-1".into(),
    };
    let response = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/runs/child-run/input"
        )))
        .header("x-omini-client-id", &client_id)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(next_child_input(&mut socket).await, client_id);
    let changed = omini_protocol::AgentRunInputRequest {
        input: UserInput::plain("different"),
        client_echo_id: "echo-1".into(),
    };
    let response = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/runs/child-run/input"
        )))
        .header("x-omini-client-id", &client_id)
        .json(&changed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);

    // 第二个客户端使用相同回显 ID 和正文，仍有独立的投递键。
    let domain_input = omini_domain::conversation::UserInput {
        intent: omini_domain::input::UserInputIntent::Message,
        parts: vec![omini_domain::input::InputPart::Text {
            text: "follow up".into(),
        }],
        attachments: Vec::new(),
    };
    let peer_id = register_client(&daemon).await;
    let mut peer_request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{thread_id}/events"
        ))
        .into_client_request()
        .unwrap();
    peer_request.headers_mut().insert(
        "x-omini-client-id",
        HeaderValue::from_str(&peer_id).unwrap(),
    );
    let (mut peer_socket, _) = connect_async(peer_request).await.unwrap();
    let peer_snapshot = next_thread_snapshot(&mut peer_socket).await;
    assert_eq!(
        peer_snapshot
            .agent_tasks
            .iter()
            .find(|task| task.task.task_id == "child-run")
            .unwrap()
            .history
            .iter()
            .filter(|entry| **entry
                == omini_domain::conversation::ConversationEntry::UserInput(domain_input.clone()))
            .count(),
        1
    );
    let response = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/runs/child-run/input"
        )))
        .header("x-omini-client-id", &peer_id)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(next_child_input(&mut socket).await, peer_id);
    assert_eq!(next_child_input(&mut peer_socket).await, peer_id);
    for client in [&client_id, &peer_id] {
        let source = ClientMessage {
            client_id: client.clone(),
            client_echo_id: "echo-1".into(),
            input: domain_input.clone(),
        };
        assert!(
            db.client_delivery("child-run", &source)
                .await
                .unwrap()
                .is_some()
        );
    }
    // 两个客户端重连时都应从快照得到两条相同正文的独立 UI 消息。
    close_socket(&mut socket).await;
    let mut reconnect_request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{thread_id}/events"
        ))
        .into_client_request()
        .unwrap();
    reconnect_request.headers_mut().insert(
        "x-omini-client-id",
        HeaderValue::from_str(&client_id).unwrap(),
    );
    let (mut socket, _) = connect_async(reconnect_request).await.unwrap();
    let reconnected_snapshot = next_thread_snapshot(&mut socket).await;
    assert_eq!(
        reconnected_snapshot
            .agent_tasks
            .iter()
            .find(|task| task.task.task_id == "child-run")
            .unwrap()
            .history
            .iter()
            .filter(|entry| **entry
                == omini_domain::conversation::ConversationEntry::UserInput(domain_input.clone()))
            .count(),
        2
    );
    close_socket(&mut peer_socket).await;
    let mut peer_request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{thread_id}/events"
        ))
        .into_client_request()
        .unwrap();
    peer_request.headers_mut().insert(
        "x-omini-client-id",
        HeaderValue::from_str(&peer_id).unwrap(),
    );
    let (mut peer_socket, _) = connect_async(peer_request).await.unwrap();
    let peer_snapshot = next_thread_snapshot(&mut peer_socket).await;
    assert_eq!(
        peer_snapshot
            .agent_tasks
            .iter()
            .find(|task| task.task.task_id == "child-run")
            .unwrap()
            .history
            .iter()
            .filter(|entry| **entry
                == omini_domain::conversation::ConversationEntry::UserInput(domain_input.clone()))
            .count(),
        2
    );
    let cancel = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/runs/child-run/cancel"
        )))
        .header("x-omini-client-id", &client_id)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), reqwest::StatusCode::NO_CONTENT);

    let other_client = register_client(&daemon).await;
    let mut other_request = daemon
        .websocket_url(&format!(
            "/projects/{project_id}/threads/{}/events",
            other.thread_id
        ))
        .into_client_request()
        .unwrap();
    other_request.headers_mut().insert(
        "x-omini-client-id",
        HeaderValue::from_str(&other_client).unwrap(),
    );
    let (mut other_socket, _) = connect_async(other_request).await.unwrap();
    let response = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{}/runs/child-run/input",
            other.thread_id
        )))
        .header("x-omini-client-id", &other_client)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    // 则终态的新来源键被拒绝，已接受的来源键由投递状态决定。
    db.finish_agent_task(
        "child-run",
        omini_domain::task::TaskStatus::Completed,
        &omini_runtime_contract::thread_domain::AgentTaskResult {
            output: None,
            error: None,
            warnings: Vec::new(),
            undelivered_messages: None,
        },
        fixed_time(),
    )
    .await
    .unwrap();
    let terminal_input = omini_protocol::AgentRunInputRequest {
        client_echo_id: "echo-2".into(),
        ..input
    };
    let response = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/runs/child-run/input"
        )))
        .header("x-omini-client-id", &client_id)
        .json(&terminal_input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    close_socket(&mut socket).await;
    close_socket(&mut peer_socket).await;
    close_socket(&mut other_socket).await;
    daemon.shutdown().await;
}

#[tokio::test]
async fn threads_run_requires_connected_client() {
    let mut daemon = support::TestDaemon::start("thread-auth").await;
    let (project_id, thread_id) = project_and_thread(&daemon).await;
    let request = SubmitRunRequest::SubmitMessage {
        input: UserInput::plain("hello"),
        client_echo_id: None,
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
async fn threads_run_returns_accepted_with_run_id() {
    let mut daemon = support::TestDaemon::start("thread-run-accepted").await;
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
    // 等待初始状态帧，确保服务端已经登记该客户端的连接和控制权。
    for _ in 0..7 {
        socket
            .next()
            .await
            .expect("initial WebSocket envelope should arrive")
            .expect("initial WebSocket frame should be valid");
    }

    let (status, submitted): (_, RunSubmittedResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/runs"),
            Some(&client_id),
            &SubmitRunRequest::SubmitMessage {
                input: UserInput::plain("hello"),
                client_echo_id: None,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    assert!(!submitted.run_id.is_empty());

    drop(socket);
    daemon.shutdown().await;
}

#[tokio::test]
async fn duplicate_connections_are_refcounted() {
    let mut daemon = support::TestDaemon::start("thread-duplicate-client").await;
    let (project_id, thread_id) = project_and_thread(&daemon).await;
    let client_id = register_client(&daemon).await;
    let request = || {
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
        request
    };

    let (mut first, _) = connect_async(request())
        .await
        .expect("first WebSocket should connect");
    let (mut second, _) = connect_async(request())
        .await
        .expect("second WebSocket should connect");
    for socket in [&mut first, &mut second] {
        for _ in 0..7 {
            tokio::time::timeout(std::time::Duration::from_secs(1), socket.next())
                .await
                .expect("initial WebSocket envelope should arrive")
                .expect("WebSocket should remain open")
                .expect("initial WebSocket frame should be valid");
        }
    }

    // 连接数按 client_id 去重展示，但在线状态必须保留底层 WebSocket 引用计数。
    let (status, response): (_, ThreadRuntimeStatus) = daemon
        .get(&format!(
            "/projects/{project_id}/threads/{thread_id}/status"
        ))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(response.connected_client_count, 1);

    close_socket(&mut first).await;
    let (status, lease): (_, ControllerLease) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/controller/claim"),
            Some(&client_id),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(lease.controller_id.as_deref(), Some(client_id.as_str()));

    // 只有最后一条连接关闭后，这个 client_id 才真正离线。
    close_socket(&mut second).await;
    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/controller/claim"),
            Some(&client_id),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(error.code, "client_not_connected");

    let (mut reconnected, _) = connect_async(request())
        .await
        .expect("same client should reconnect");
    let mut resumed_status = None;
    for _ in 0..7 {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), reconnected.next())
            .await
            .expect("reconnect envelope should arrive")
            .expect("reconnected WebSocket should remain open")
            .expect("reconnect frame should be valid")
            .into_text()
            .expect("reconnect frame should be text");
        if let ServerEnvelope::RuntimeStatus { status } =
            serde_json::from_str::<ServerEnvelope>(&message).expect("envelope should decode")
        {
            resumed_status = Some(status);
        }
    }
    let resumed_status = resumed_status.expect("reconnect should include runtime status");
    assert_eq!(resumed_status.thread_id, thread_id);
    assert_eq!(resumed_status.connected_client_count, 1);
    assert_eq!(
        resumed_status.controller_id.as_deref(),
        Some(client_id.as_str())
    );
    close_socket(&mut reconnected).await;

    daemon.shutdown().await;
}

#[tokio::test]
async fn threads_invalid_run_references_reject_before_accept() {
    let mut daemon = support::TestDaemon::start("thread-unknown-skill").await;
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

    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/runs"),
            Some(&client_id),
            &SubmitRunRequest::SubmitMessage {
                input: UserInput {
                    parts: vec![InputPart::Skill {
                        name: "not-installed".to_string(),
                    }],
                    attachment_ids: Vec::new(),
                },
                client_echo_id: None,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error.code, "skill_not_found");
    assert_eq!(error.message, "skill 'not-installed' does not exist");

    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/runs"),
            Some(&client_id),
            &SubmitRunRequest::SubmitMessage {
                input: UserInput {
                    parts: vec![InputPart::Text {
                        text: "look".to_string(),
                    }],
                    attachment_ids: vec![uuid::Uuid::new_v4().to_string()],
                },
                client_echo_id: None,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(error.code, "attachment_not_found");

    drop(socket);
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
    daemon
        .send_no_content(
            Method::POST,
            &format!("/projects/{project_id}/threads/{thread_id}/rename"),
            Some(&client_id),
            &RenameThreadRequest {
                title: requested_title,
            },
        )
        .await;

    let (status, threads): (_, ThreadsResponse) =
        daemon.get(&format!("/projects/{project_id}/threads")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(threads.threads.len(), 1);
    assert_eq!(threads.threads[0].id, thread_id);
    assert_eq!(threads.threads[0].title, "界".repeat(300));

    let image = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    let response = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/attachments"
        )))
        .header("x-omini-client-id", &client_id)
        .multipart(
            reqwest::multipart::Form::new().part(
                "file",
                reqwest::multipart::Part::bytes(image.clone())
                    .file_name("diagram.png")
                    .mime_str("image/png")
                    .expect("MIME type should parse"),
            ),
        )
        .send()
        .await;
    let response = response.expect("attachment request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let uploaded = response
        .json::<AttachmentUploadResponse>()
        .await
        .expect("attachment response should decode");
    uuid::Uuid::parse_str(&uploaded.attachment_id).expect("attachment ID should be opaque UUID");
    let duplicate = daemon
        .client()
        .post(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/attachments"
        )))
        .header("x-omini-client-id", &client_id)
        .multipart(
            reqwest::multipart::Form::new().part(
                "file",
                reqwest::multipart::Part::bytes(image.clone())
                    .file_name("diagram-copy.png")
                    .mime_str("image/png")
                    .expect("MIME type should parse"),
            ),
        )
        .send()
        .await
        .expect("duplicate attachment upload should complete")
        .json::<AttachmentUploadResponse>()
        .await
        .expect("duplicate attachment response should decode");
    assert_ne!(duplicate.attachment_id, uploaded.attachment_id);

    let response = daemon
        .client()
        .get(daemon.url(&format!(
            "/projects/{project_id}/threads/{thread_id}/attachments/{}",
            uploaded.attachment_id
        )))
        .send()
        .await
        .expect("attachment GET should complete");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(
        response.headers()["content-length"],
        image.len().to_string()
    );
    assert_eq!(
        response.headers()["content-disposition"],
        "inline; filename=\"diagram.png\""
    );
    assert!(
        response.headers()["etag"]
            .to_str()
            .unwrap()
            .starts_with('"')
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response.bytes().await.expect("attachment body should load"),
        image
    );

    let (status, other_thread): (_, omini_protocol::CreateThreadResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/threads"),
            None,
            &CreateThreadRequest::default(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::CREATED);
    let response = daemon
        .client()
        .get(daemon.url(&format!(
            "/projects/{project_id}/threads/{}/attachments/{}",
            other_thread.thread_id, uploaded.attachment_id
        )))
        .send()
        .await
        .expect("cross-thread attachment GET should complete");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    let (status, statuses): (_, ThreadStatusesResponse) = daemon
        .get(&format!(
            "/projects/{project_id}/threads/statuses?status=idle,%20working"
        ))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let status = statuses
        .statuses
        .iter()
        .find(|status| status.thread_id == thread_id)
        .expect("original thread status should be present");
    assert_eq!(status.state, omini_protocol::ThreadRuntimeState::Idle);

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
