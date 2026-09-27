use crate::app::event::RuntimeToUiEvent;
use omini_protocol as protocol;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::*;
pub fn spawn_project_client(
    connection: ProjectConnection,
    event_tx: mpsc::Sender<RuntimeToUiEvent>,
    request_rx: mpsc::Receiver<ClientRequest>,
) -> JoinHandle<()> {
    // 客户端传输层独立运行；错误统一转回 RuntimeToUiEvent，避免 UI 线程直接处理网络细节。
    tokio::spawn(async move {
        if let Err(err) = run_project_client(connection, event_tx.clone(), request_rx).await {
            let _ = event_tx
                .send(RuntimeToUiEvent::error(format!(
                    "Runtime client disconnected: {err}"
                )))
                .await;
        }
    })
}

pub async fn run_project_client(
    mut connection: ProjectConnection,
    event_tx: mpsc::Sender<RuntimeToUiEvent>,
    mut request_rx: mpsc::Receiver<ClientRequest>,
) -> Result<(), String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|err| format!("build HTTP client: {err}"))?;
    let mut active_thread_id: Option<String> = None;
    let mut pending_requests: VecDeque<ClientRequest> = VecDeque::new();
    let mut blank_profile = protocol::ActiveProfile::Main;
    // 每个活跃 thread 只允许一次最新 daemon 地址重连；否则健康检查成功但 thread
    // 仍不可用时会在同一个断线点空转。
    let mut refreshed_active_thread = false;

    loop {
        if let Some(thread_id) = active_thread_id.take() {
            // 一次只维护一个活跃 thread WebSocket；切换线程时结束旧 loop 再连接新 loop。
            let mut thread_initial_requests = std::mem::take(&mut pending_requests);
            let disconnect = match run_connected_thread(
                &http,
                &mut connection,
                &thread_id,
                &event_tx,
                &mut request_rx,
                &mut thread_initial_requests,
            )
            .await
            {
                Ok(ThreadLoop::Switch(next_thread_id)) => {
                    active_thread_id = Some(next_thread_id);
                    refreshed_active_thread = false;
                    continue;
                }
                Ok(ThreadLoop::Blank(profile)) => {
                    active_thread_id = None;
                    blank_profile = profile;
                    refreshed_active_thread = false;
                    continue;
                }
                Ok(ThreadLoop::Closed(reason)) | Err(reason) => reason,
            };

            if refreshed_active_thread {
                // 已经用最新地址重连过一次，第二次断开就直接报告，避免隐藏真实不可恢复错误。
                let _ = event_tx
                    .send(RuntimeToUiEvent::error(format!(
                        "Runtime client disconnected: {disconnect}"
                    )))
                    .await;
                refreshed_active_thread = false;
            } else {
                match reconnect_latest_daemon(&http, &mut connection).await {
                    Ok(()) => {
                        // 新 daemon 不认识旧 client_id；rediscovery 会重新注册并按 UUID open 项目。
                        active_thread_id = Some(thread_id);
                        pending_requests = thread_initial_requests;
                        refreshed_active_thread = true;
                    }
                    Err(reconnect_err) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::error(format!(
                                "Runtime client disconnected: {disconnect}; reconnect failed: {reconnect_err}"
                            )))
                            .await;
                        refreshed_active_thread = false;
                    }
                }
            }
            continue;
        }

        let Some(request) = request_rx.recv().await else {
            break;
        };
        // 没有活跃 thread 时，项目级请求可以直接处理；线程级请求会先创建 thread 再补发。
        match handle_project_request(
            &http,
            &mut connection,
            request,
            &event_tx,
            &mut blank_profile,
        )
        .await?
        {
            ProjectAction::None => {}
            ProjectAction::Connect { thread_id, pending } => {
                active_thread_id = Some(thread_id);
                pending_requests = pending;
                refreshed_active_thread = false;
            }
            ProjectAction::Shutdown => break,
        }
    }

    Ok(())
}

pub enum ProjectAction {
    None,
    // Connect 可能带一个待补发请求，用于“用户第一次输入时自动创建线程并立刻提交”。
    Connect {
        thread_id: String,
        pending: VecDeque<ClientRequest>,
    },
    Shutdown,
}

pub enum ThreadLoop {
    Switch(String),
    Blank(protocol::ActiveProfile),
    Closed(String),
}

pub enum LocalAction {
    None,
    Switch(String),
    Blank(protocol::ActiveProfile),
}
