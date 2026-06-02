//! 会话级 WebSocket 事件流。
//!
//! 这个模块负责把持久化 snapshot、运行中 replay、runtime fanout 和 controller 状态变化
//! 统一编码成 `ServerEnvelope` 发给单个客户端连接。

use crate::runtime::{RuntimeSession, SessionManager};
use axum::extract::ws::{Message as AxumMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use omini_protocol::{RuntimeEvent, ServerEnvelope};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{Duration, timeout};

/// 处理单个客户端订阅会话事件流的 WebSocket 生命周期。
pub(crate) async fn handle_socket(
    socket: WebSocket,
    manager: Arc<SessionManager>,
    session: Arc<RuntimeSession>,
    session_id: String,
    client_id: String,
) {
    // 先订阅再登记连接，避免 controller 变化或 runtime 事件夹在连接建立过程中丢失。
    let mut events = session.subscribe();
    let mut server_events = session.subscribe_server_events();
    let mut controller_events = session.subscribe_controller();
    let mut replay_through = 0u64;
    let (mut write, mut read) = socket.split();
    let controller_id = session.register_client_connection(client_id.clone()).await;
    let mut role = session.client_role(&client_id).await;

    // 新连接先拿到控制权状态和自己的角色，TUI 才能在后续快照到来前正确渲染输入态。
    let controller = ServerEnvelope::ControllerChanged {
        controller_id: controller_id.clone(),
    };
    let _ = send_axum_envelope(&mut write, &controller).await;
    let role_envelope = ServerEnvelope::ClientRoleChanged {
        client_id: client_id.clone(),
        role,
        controller_id,
    };
    let _ = send_axum_envelope(&mut write, &role_envelope).await;

    // snapshot 来自持久化历史，先发它能让重连客户端立刻恢复一个完整会话视图。
    match session.current_snapshot_events().await {
        Ok(events) => {
            for event in events {
                let envelope = ServerEnvelope::Event { event };
                if send_axum_envelope(&mut write, &envelope).await.is_err() {
                    session.unregister_client_connection(&client_id).await;
                    manager.close_session_if_idle(&session_id, &session).await;
                    return;
                }
            }
        }
        Err(error) => {
            let _ = send_notification(&mut write, "error", error.message()).await;
        }
    }

    // snapshot 不要求 core 已完成加载；随后再短暂等待 core hydrate，失败时只通知客户端。
    match timeout(Duration::from_secs(5), session.ensure_loaded()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = send_notification(&mut write, "error", error.message()).await;
        }
        Err(_) => {
            let _ = send_notification(
                &mut write,
                "warn",
                "Session snapshot was shown, but runtime loading is taking longer than expected",
            )
            .await;
        }
    }

    // replay 只包含运行中尚未被 snapshot 覆盖的尾部事件；记录 seq 用于过滤订阅流里的重复事件。
    let replay_events = session.replay_events().await;
    for event in replay_events {
        replay_through = replay_through.max(event.seq);
        let envelope = ServerEnvelope::Event { event: event.event };
        if send_axum_envelope(&mut write, &envelope).await.is_err() {
            session.unregister_client_connection(&client_id).await;
            manager.close_session_if_idle(&session_id, &session).await;
            return;
        }
    }

    // 持久化 snapshot 只能恢复消息/配置；replay 可能包含 run_started 等生命周期事件，
    // 所以实时状态要在 replay 后同步，作为新连接初始化阶段的最终计时校准。
    let status = ServerEnvelope::RuntimeStatus {
        status: session.runtime_status().await,
    };
    if send_axum_envelope(&mut write, &status).await.is_err() {
        session.unregister_client_connection(&client_id).await;
        manager.close_session_if_idle(&session_id, &session).await;
        return;
    }

    loop {
        tokio::select! {
            // 客户端目前不通过 WebSocket 发送业务命令，只处理连接控制帧。
            message = read.next() => {
                let Some(Ok(message)) = message else {
                    break;
                };
                match message {
                    AxumMessage::Close(_) => break,
                    AxumMessage::Ping(payload) => {
                        if write.send(AxumMessage::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    AxumMessage::Text(_) | AxumMessage::Pong(_) | AxumMessage::Binary(_) => {}
                }
            }
            controller = controller_events.recv() => {
                match controller {
                    Ok(controller_id) => {
                        let envelope = ServerEnvelope::ControllerChanged { controller_id };
                        if send_axum_envelope(&mut write, &envelope).await.is_err() {
                            break;
                        }
                        // controller 变化不一定改变当前客户端角色，只有变化时才发角色事件。
                        let next_role = session.client_role(&client_id).await;
                        if next_role != role {
                            role = next_role;
                            let envelope = ServerEnvelope::ClientRoleChanged {
                                client_id: client_id.clone(),
                                role,
                                controller_id: session.controller_id().await,
                            };
                            if send_axum_envelope(&mut write, &envelope).await.is_err() {
                                break;
                            }
                        }
                    }
                    // controller 事件是状态广播，丢中间值没关系，下一次变化会带来最新 controller_id。
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        // replay 后订阅流可能继续吐出同一批事件，按 seq 去重避免 UI 重复追加。
                        if event.seq <= replay_through {
                            continue;
                        }
                        let envelope = ServerEnvelope::Event { event: event.event };
                        if send_axum_envelope(&mut write, &envelope).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // runtime 内容事件丢失会影响对话连续性，明确通知客户端显示告警。
                        let envelope = ServerEnvelope::Event {
                            event: RuntimeEvent::new(
                                "notification",
                                serde_json::json!({
                                    "type": "notification",
                                    "kind": "warn",
                                    "message": "Runtime event stream lagged; some events were dropped",
                                    "details": [],
                                }),
                            ),
                        };
                        let _ = send_axum_envelope(&mut write, &envelope).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            event = server_events.recv() => {
                match event {
                    Ok(event) => {
                        let envelope = ServerEnvelope::Event { event };
                        if send_axum_envelope(&mut write, &envelope).await.is_err() {
                            break;
                        }
                    }
                    // server event 多为派生通知，丢中间项不需要打断 socket。
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    session.unregister_client_connection(&client_id).await;
    manager.close_session_if_idle(&session_id, &session).await;
}

/// 将协议 envelope 序列化为 WebSocket 文本帧。
async fn send_axum_envelope<S>(sink: &mut S, envelope: &ServerEnvelope) -> Result<(), String>
where
    S: futures_util::Sink<AxumMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let text = serde_json::to_string(envelope).map_err(|err| err.to_string())?;
    sink.send(AxumMessage::Text(text.into()))
        .await
        .map_err(|err| err.to_string())
}

/// 连接初始化阶段直接向客户端发送一条 runtime notification。
async fn send_notification<S>(sink: &mut S, kind: &str, message: &str) -> Result<(), String>
where
    S: futures_util::Sink<AxumMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let envelope = ServerEnvelope::Event {
        event: RuntimeEvent::new(
            "notification",
            serde_json::json!({
                "type": "notification",
                "kind": kind,
                "message": message,
                "details": [],
            }),
        ),
    };
    send_axum_envelope(sink, &envelope).await
}
