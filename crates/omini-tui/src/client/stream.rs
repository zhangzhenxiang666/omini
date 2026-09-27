use crate::app::event::RuntimeToUiEvent;
use futures_util::{SinkExt, StreamExt};
use omini_protocol as protocol;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::client::*;
pub async fn run_connected_thread(
    http: &reqwest::Client,
    connection: &mut ProjectConnection,
    thread_id: &str,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    request_rx: &mut mpsc::Receiver<ClientRequest>,
    initial_requests: &mut VecDeque<ClientRequest>,
) -> Result<ThreadLoop, String> {
    let base = thread_base_url(connection, thread_id);
    let url = format!(
        "ws://{}/v1/projects/{}/threads/{thread_id}/events",
        connection.addr, connection.project_id
    );
    let mut ws_request = url
        .as_str()
        .into_client_request()
        .map_err(|err| format!("build websocket request {url}: {err}"))?;
    let client_header = HeaderValue::from_str(&connection.client_id)
        .map_err(|err| format!("build websocket client id header: {err}"))?;
    ws_request
        .headers_mut()
        .insert(CLIENT_ID_HEADER, client_header);
    // WebSocket 只承载 server event；用户动作仍走 HTTP，二者共享同一个 client_id 权限身份。
    let (socket, _) = timeout(Duration::from_secs(10), connect_async(ws_request))
        .await
        .map_err(|_| format!("connect {url}: timed out"))?
        .map_err(|err| format!("connect {url}: {err}"))?;
    let (mut write, mut read) = socket.split();
    let client_id = connection.client_id.clone();
    let mut did_calibrate_initial_status = false;

    while let Some(request) = initial_requests.pop_front() {
        match handle_local_request(
            http, connection, thread_id, &base, &client_id, request, event_tx,
        )
        .await?
        {
            LocalAction::None => {}
            LocalAction::Switch(next_thread_id) => {
                // pending request 也可能是打开线程，出现时直接切到目标 thread。
                return Ok(ThreadLoop::Switch(next_thread_id));
            }
            LocalAction::Blank(profile) => return Ok(ThreadLoop::Blank(profile)),
        }
    }

    loop {
        tokio::select! {
            // 本地交互转成 HTTP 请求；如果请求要求切换线程，退出当前 WebSocket loop。
            Some(request) = request_rx.recv() => {
                match handle_local_request(
                    http,
                    connection,
                    thread_id,
                    &base,
                    &client_id,
                    request,
                    event_tx,
                )
                .await
                {
                    Ok(LocalAction::None) => {}
                    Ok(LocalAction::Switch(next_thread_id)) => {
                        return Ok(ThreadLoop::Switch(next_thread_id));
                    }
                    Ok(LocalAction::Blank(profile)) => return Ok(ThreadLoop::Blank(profile)),
                    Err(err) => {
                        let _ = event_tx.send(RuntimeToUiEvent::error(err)).await;
                    }
                }
            }
            // WebSocket 流只向 UI 注入 server 事件，连接控制帧在这里就地处理。
            message = read.next() => {
                let Some(message) = message else {
                    return Ok(ThreadLoop::Closed("server event stream ended".to_string()));
                };
                let message = message.map_err(|err| format!("read server message: {err}"))?;
                match message {
                    TungsteniteMessage::Text(text) => {
                        match handle_server_text(text.as_str(), event_tx).await? {
                            HandleOutcome::PassThrough => {}
                            HandleOutcome::ModelChanged {
                                provider,
                                model,
                                thinking_effort,
                                context_window,
                            } => {
                                // 服务端发来的 ModelChanged 事件（如打开已有 thread 时），
                                // 同步更新 open snapshot，确保 /new 后创建新 thread
                                // 沿用正确的 provider/model/effort。
                                connection.open.active_provider = provider;
                                connection.open.model = model;
                                connection.open.thinking_effort = thinking_effort;
                                connection.open.context_window = context_window;
                            }
                            HandleOutcome::SawRuntimeStatus => {
                                if !did_calibrate_initial_status {
                                    did_calibrate_initial_status = true;
                                    // WS 初始化 status 先让 UI 立刻恢复；随后只测一次 HTTP status
                                    // 往返，避免把 snapshot/replay/hydrate 时间算进运行耗时。
                                    if let Some(status) =
                                        fetch_calibrated_runtime_status(http, &base).await
                                    {
                                        event_tx
                                            .send(RuntimeToUiEvent::RuntimeStatusSynced {
                                                status,
                                                restore_pending_pauses: false,
                                            })
                                            .await
                                            .map_err(|_| "TUI event receiver closed".to_string())?;
                                    }
                                }
                            }
                            HandleOutcome::Switch(next_thread_id) => {
                                return Ok(ThreadLoop::Switch(next_thread_id));
                            }
                        }
                    }
                    TungsteniteMessage::Close(_) => {
                        return Ok(ThreadLoop::Closed(
                            "server closed the event stream".to_string(),
                        ));
                    }
                    TungsteniteMessage::Ping(payload) => {
                        write
                            .send(TungsteniteMessage::Pong(payload))
                            .await
                            .map_err(|err| format!("send pong: {err}"))?;
                    }
                    TungsteniteMessage::Pong(_) | TungsteniteMessage::Binary(_) | TungsteniteMessage::Frame(_) => {}
                }
            }
        }
    }
}

pub async fn handle_server_text(
    text: &str,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<HandleOutcome, String> {
    match serde_json::from_str::<protocol::ServerEnvelope>(text)
        .map_err(|err| format!("decode server envelope: {err}"))?
    {
        protocol::ServerEnvelope::Event { event } => {
            // 「在新线程中执行计划」:server fork 出新 thread 后通过普通 runtime
            // 事件通道广播 ThreadSwitched。复用具名 `Switch` 控制流——主循环
            // 断开旧 ws 并按新 id 重建,不要把它当作普通 runtime 事件投递给 UI。
            if let protocol::TypedRuntimeEvent::ThreadSwitched(payload) = event.event {
                return Ok(HandleOutcome::Switch(payload.to));
            }
            // ModelChanged 需要同步更新 open snapshot，在转发给 UI 之前先
            // 记下模型信息,返回非默认 outcome 让主循环同步缓存。
            let model_changed = match &event.event {
                protocol::TypedRuntimeEvent::ModelChanged(payload) => {
                    Some(HandleOutcome::ModelChanged {
                        provider: payload.provider.clone(),
                        model: payload.model.clone(),
                        thinking_effort: payload.thinking_effort,
                        context_window: payload.context_window,
                    })
                }
                _ => None,
            };
            let event = runtime_event_from_protocol(event);
            event_tx
                .send(event)
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(model_changed.unwrap_or(HandleOutcome::PassThrough))
        }
        protocol::ServerEnvelope::RuntimeStatus { status } => {
            event_tx
                .send(RuntimeToUiEvent::RuntimeStatusSynced {
                    status,
                    restore_pending_pauses: true,
                })
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(HandleOutcome::SawRuntimeStatus)
        }
        // controller/role envelope 先作为协议能力保留，当前 TUI 渲染还主要依赖 runtime status。
        protocol::ServerEnvelope::ControllerChanged { .. } => Ok(HandleOutcome::PassThrough),
        protocol::ServerEnvelope::ClientRoleChanged { .. } => Ok(HandleOutcome::PassThrough),
    }
}

/// ws 文本帧处理的结果,决定下一步动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleOutcome {
    /// 普通事件,继续主循环。
    PassThrough,
    /// 收到了 RuntimeStatus,主循环在初始化阶段据此校准一次 HTTP /status 查询。
    SawRuntimeStatus,
    /// 收到 ThreadSwitched,主循环断开旧 ws 并按新 id 重新连接。
    Switch(String),
    /// 收到 ModelChanged，主循环更新 open snapshot 保持与服务端同步。
    ModelChanged {
        provider: String,
        model: String,
        thinking_effort: Option<omini_protocol::ThinkingEffort>,
        context_window: Option<u32>,
    },
}
