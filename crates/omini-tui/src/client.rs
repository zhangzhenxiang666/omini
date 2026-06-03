use crate::types::events::{RuntimeToUiEvent, SessionUsageSnapshot};
use futures_util::{SinkExt, StreamExt};
use omini_protocol as protocol;
use omini_protocol::ProtocolError;
use reqwest::Method;
use serde::Deserialize;
use std::collections::VecDeque;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

const CLIENT_ID_HEADER: &str = "x-omini-client-id";

// TUI 内部意图先收敛成 ClientRequest，再由这一层翻译成 HTTP/WS 协议调用。
#[derive(Debug, Clone)]
pub(crate) enum ClientRequest {
    RunSubmitUserInput {
        input: protocol::UserInput,
    },
    RunInterveneInput {
        input: protocol::UserInput,
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
    OpenSessionPicker,
    SessionOpen {
        session_id: String,
    },
    SessionNew {
        profile: protocol::ActiveProfile,
    },
    SessionRename {
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
        plan_id: String,
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
        model: Option<String>,
    },
    ExpandSkillRun {
        skill_name: String,
        prompt: String,
        input: Option<protocol::UserInput>,
    },
    ThinkingDisplaySet {
        show: Option<bool>,
    },
    AppShutdown,
}

#[derive(Debug)]
pub struct ProjectConnection {
    pub addr: SocketAddr,
    pub project_id: String,
    // client_id 同时用于 HTTP header 和 WebSocket header，server 用它判断 controller 权限。
    pub client_id: String,
    pub attach: protocol::ProjectAttachResponse,
}

pub(crate) fn spawn_project_client(
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

async fn run_project_client(
    mut connection: ProjectConnection,
    event_tx: mpsc::Sender<RuntimeToUiEvent>,
    mut request_rx: mpsc::Receiver<ClientRequest>,
) -> Result<(), String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|err| format!("build HTTP client: {err}"))?;
    let mut active_session_id: Option<String> = None;
    let mut pending_requests: VecDeque<ClientRequest> = VecDeque::new();
    let mut blank_profile = protocol::ActiveProfile::Main;
    // 每个活跃 session 只允许一次最新 daemon 地址重连；否则健康检查成功但 session
    // 仍不可用时会在同一个断线点空转。
    let mut refreshed_active_session = false;

    loop {
        if let Some(session_id) = active_session_id.take() {
            // 一次只维护一个活跃 session WebSocket；切换会话时结束旧 loop 再连接新 loop。
            let mut session_initial_requests = std::mem::take(&mut pending_requests);
            let disconnect = match run_connected_session(
                &http,
                &connection,
                &session_id,
                &event_tx,
                &mut request_rx,
                &mut session_initial_requests,
            )
            .await
            {
                Ok(SessionLoop::Switch(next_session_id)) => {
                    active_session_id = Some(next_session_id);
                    refreshed_active_session = false;
                    continue;
                }
                Ok(SessionLoop::Blank(profile)) => {
                    active_session_id = None;
                    blank_profile = profile;
                    refreshed_active_session = false;
                    continue;
                }
                Ok(SessionLoop::Closed(reason)) | Err(reason) => reason,
            };

            if refreshed_active_session {
                // 已经用最新地址重连过一次，第二次断开就直接报告，避免隐藏真实不可恢复错误。
                let _ = event_tx
                    .send(RuntimeToUiEvent::error(format!(
                        "Runtime client disconnected: {disconnect}"
                    )))
                    .await;
                refreshed_active_session = false;
            } else {
                match reconnect_latest_daemon(&http, &mut connection).await {
                    Ok(()) => {
                        // 新 daemon 不认识旧 client_id；rediscovery 会重新注册并 attach 项目。
                        active_session_id = Some(session_id);
                        pending_requests = session_initial_requests;
                        refreshed_active_session = true;
                    }
                    Err(reconnect_err) => {
                        let _ = event_tx
                            .send(RuntimeToUiEvent::error(format!(
                                "Runtime client disconnected: {disconnect}; reconnect failed: {reconnect_err}"
                            )))
                            .await;
                        refreshed_active_session = false;
                    }
                }
            }
            continue;
        }

        let Some(request) = request_rx.recv().await else {
            break;
        };
        // 没有活跃 session 时，项目级请求可以直接处理；会话级请求会先创建 session 再补发。
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
            ProjectAction::Connect {
                session_id,
                pending,
            } => {
                active_session_id = Some(session_id);
                pending_requests = pending;
                refreshed_active_session = false;
            }
            ProjectAction::Shutdown => break,
        }
    }

    Ok(())
}

enum ProjectAction {
    None,
    // Connect 可能带一个待补发请求，用于“用户第一次输入时自动创建会话并立刻提交”。
    Connect {
        session_id: String,
        pending: VecDeque<ClientRequest>,
    },
    Shutdown,
}

enum SessionLoop {
    Switch(String),
    Blank(protocol::ActiveProfile),
    Closed(String),
}

enum LocalAction {
    None,
    Switch(String),
    Blank(protocol::ActiveProfile),
}

async fn handle_project_request(
    http: &reqwest::Client,
    connection: &mut ProjectConnection,
    request: ClientRequest,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    blank_profile: &mut protocol::ActiveProfile,
) -> Result<ProjectAction, String> {
    match request {
        ClientRequest::RunSubmitUserInput { .. } | ClientRequest::ExpandSkillRun { .. } => {
            let session_id = create_session(http, connection, *blank_profile, event_tx).await?;
            let mut pending = VecDeque::new();
            pending.push_back(request);
            Ok(ProjectAction::Connect {
                session_id,
                pending,
            })
        }
        ClientRequest::OpenSessionPicker => {
            let sessions: protocol::SessionsResponse =
                get_json(http, &project_sessions_url(connection)).await?;
            event_tx
                .send(RuntimeToUiEvent::InteractionRequest(
                    event_types_session_selection(sessions),
                ))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::None)
        }
        ClientRequest::SessionOpen { session_id } => Ok(ProjectAction::Connect {
            session_id,
            pending: VecDeque::new(),
        }),
        ClientRequest::SessionNew { profile } => {
            *blank_profile = profile;
            emit_blank_session(event_tx, connection).await?;
            event_tx
                .send(RuntimeToUiEvent::ActiveProfileChanged(profile))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::None)
        }
        ClientRequest::OpenModelPicker => {
            let models: protocol::ModelsResponse =
                get_json(http, &project_models_url(connection)).await?;
            event_tx
                .send(RuntimeToUiEvent::InteractionRequest(
                    event_types_model_selection(models),
                ))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::None)
        }
        ClientRequest::ModelSelect {
            provider,
            model,
            thinking_effort,
        } => {
            let config: protocol::ProjectRuntimeConfigResponse = post_json_without_client(
                http,
                &project_model_url(connection),
                &protocol::SetModelRequest {
                    provider,
                    model,
                    thinking_effort,
                },
            )
            .await?;
            apply_project_runtime_config(connection, event_tx, config).await?;
            Ok(ProjectAction::None)
        }
        ClientRequest::ModelThinkingEffortSet { effort } => {
            let config: protocol::ProjectRuntimeConfigResponse = post_json_without_client(
                http,
                &project_thinking_effort_url(connection),
                &protocol::SetThinkingEffortRequest { effort },
            )
            .await?;
            apply_project_runtime_config(connection, event_tx, config).await?;
            Ok(ProjectAction::None)
        }
        ClientRequest::ProfileToggle => {
            *blank_profile = toggle_profile(*blank_profile);
            event_tx
                .send(RuntimeToUiEvent::ActiveProfileChanged(*blank_profile))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::None)
        }
        ClientRequest::ProfileSet { profile } => {
            *blank_profile = profile;
            event_tx
                .send(RuntimeToUiEvent::ActiveProfileChanged(profile))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::None)
        }
        ClientRequest::ThinkingDisplaySet { show } => {
            let config: protocol::ProjectRuntimeConfigResponse = post_json_without_client(
                http,
                &project_thinking_display_url(connection),
                &protocol::SetThinkingDisplayRequest { show },
            )
            .await?;
            apply_project_runtime_config(connection, event_tx, config).await?;
            Ok(ProjectAction::None)
        }
        ClientRequest::AppShutdown => {
            event_tx
                .send(RuntimeToUiEvent::Shutdown)
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::Shutdown)
        }
        other => {
            event_tx
                .send(RuntimeToUiEvent::error(format!(
                    "当前没有活跃会话，不能执行 {}；请先发送消息创建会话或用 /sessions 打开已有会话",
                    request_name(&other)
                )))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::None)
        }
    }
}

async fn create_session(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    profile: protocol::ActiveProfile,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<String, String> {
    let response: protocol::CreateSessionResponse = post_json_without_client(
        http,
        &project_sessions_url(connection),
        &protocol::CreateSessionRequest {
            provider: Some(connection.attach.active_provider.clone()),
            model: Some(connection.attach.model.clone()),
            thinking_effort: connection.attach.thinking_effort,
            profile: Some(profile),
        },
    )
    .await?;
    let session_id = response
        .session_id
        .ok_or_else(|| "Server did not return a session id".to_string())?;
    event_tx
        .send(RuntimeToUiEvent::ActiveProfileChanged(profile))
        .await
        .map_err(|_| "TUI event receiver closed".to_string())?;
    Ok(session_id)
}

async fn emit_blank_session(
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    connection: &ProjectConnection,
) -> Result<(), String> {
    event_tx
        .send(RuntimeToUiEvent::SessionChanged {
            session_id: None,
            messages: Vec::new(),
            subagents: Vec::new(),
            usage: SessionUsageSnapshot {
                context_window: connection.attach.context_window,
                ..SessionUsageSnapshot::default()
            },
        })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())?;
    event_tx
        .send(RuntimeToUiEvent::SessionTitleChanged { title: None })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())
}

async fn apply_project_runtime_config(
    connection: &mut ProjectConnection,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    config: protocol::ProjectRuntimeConfigResponse,
) -> Result<(), String> {
    connection.attach.active_provider = config.active_provider.clone();
    connection.attach.model = config.model.clone();
    connection.attach.thinking_effort = config.thinking_effort;
    connection.attach.context_window = config.context_window;
    connection.attach.show_thinking_blocks = config.show_thinking_blocks;

    event_tx
        .send(RuntimeToUiEvent::ModelChanged {
            provider: config.active_provider,
            model: config.model,
            thinking_effort: config.thinking_effort.map(thinking_effort_from_protocol),
            context_window: config.context_window,
        })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())?;
    event_tx
        .send(RuntimeToUiEvent::ThinkingDisplayChanged {
            show: config.show_thinking_blocks,
        })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())
}

fn toggle_profile(profile: protocol::ActiveProfile) -> protocol::ActiveProfile {
    match profile {
        protocol::ActiveProfile::Main => protocol::ActiveProfile::Auto,
        protocol::ActiveProfile::Auto => protocol::ActiveProfile::Plan,
        protocol::ActiveProfile::Plan => protocol::ActiveProfile::Main,
    }
}

fn request_name(request: &ClientRequest) -> &'static str {
    match request {
        ClientRequest::RunSubmitUserInput { .. } => "submit",
        ClientRequest::RunInterveneInput { .. } => "intervene",
        ClientRequest::RunCancel => "cancel",
        ClientRequest::ProfileToggle => "profile toggle",
        ClientRequest::ProfileSet { .. } => "profile",
        ClientRequest::OpenModelPicker => "model picker",
        ClientRequest::ModelSelect { .. } => "model select",
        ClientRequest::ModelThinkingEffortSet { .. } => "thinking effort",
        ClientRequest::OpenSessionPicker => "sessions",
        ClientRequest::SessionOpen { .. } => "session open",
        ClientRequest::SessionNew { .. } => "new session",
        ClientRequest::SessionRename { .. } => "rename",
        ClientRequest::ContextCompact { .. } => "compact",
        ClientRequest::ToolPauseResolve { .. } => "tool pause",
        ClientRequest::PlanResolve { .. } => "plan approval",
        ClientRequest::OpenAgentManager => "agents",
        ClientRequest::AgentSave { .. } => "agent save",
        ClientRequest::AgentDelete { .. } => "agent delete",
        ClientRequest::AgentGenerate { .. } => "agent generate",
        ClientRequest::ExpandSkillRun { .. } => "skill",
        ClientRequest::ThinkingDisplaySet { .. } => "thinking display",
        ClientRequest::AppShutdown => "shutdown",
    }
}

async fn run_connected_session(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    session_id: &str,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    request_rx: &mut mpsc::Receiver<ClientRequest>,
    initial_requests: &mut VecDeque<ClientRequest>,
) -> Result<SessionLoop, String> {
    let base = session_base_url(connection, session_id);
    let url = format!(
        "ws://{}/v1/projects/{}/sessions/{session_id}/events",
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
        match handle_local_request(http, connection, &base, &client_id, request, event_tx).await? {
            LocalAction::None => {}
            LocalAction::Switch(next_session_id) => {
                // pending request 也可能是打开会话，出现时直接切到目标 session。
                return Ok(SessionLoop::Switch(next_session_id));
            }
            LocalAction::Blank(profile) => return Ok(SessionLoop::Blank(profile)),
        }
    }

    loop {
        tokio::select! {
            // 本地交互转成 HTTP 请求；如果请求要求切换会话，退出当前 WebSocket loop。
            Some(request) = request_rx.recv() => {
                match handle_local_request(
                    http,
                    connection,
                    &base,
                    &client_id,
                    request,
                    event_tx,
                )
                .await
                {
                    Ok(LocalAction::None) => {}
                    Ok(LocalAction::Switch(next_session_id)) => {
                        return Ok(SessionLoop::Switch(next_session_id));
                    }
                    Ok(LocalAction::Blank(profile)) => return Ok(SessionLoop::Blank(profile)),
                    Err(err) => {
                        let _ = event_tx.send(RuntimeToUiEvent::error(err)).await;
                    }
                }
            }
            // WebSocket 流只向 UI 注入 server 事件，连接控制帧在这里就地处理。
            message = read.next() => {
                let Some(message) = message else {
                    return Ok(SessionLoop::Closed("server event stream ended".to_string()));
                };
                let message = message.map_err(|err| format!("read server message: {err}"))?;
                match message {
                    TungsteniteMessage::Text(text) => {
                        let saw_runtime_status = handle_server_text(text.as_str(), event_tx).await?;
                        if saw_runtime_status && !did_calibrate_initial_status {
                            did_calibrate_initial_status = true;
                            // WS 初始化 status 先让 UI 立刻恢复；随后只测一次 HTTP status
                            // 往返，避免把 snapshot/replay/hydrate 时间算进运行耗时。
                            if let Some(status) =
                                fetch_calibrated_runtime_status(http, &base).await
                            {
                                event_tx
                                    .send(RuntimeToUiEvent::RuntimeStatusSynced { status })
                                    .await
                                    .map_err(|_| "TUI event receiver closed".to_string())?;
                            }
                        }
                    }
                    TungsteniteMessage::Close(_) => {
                        return Ok(SessionLoop::Closed(
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

async fn handle_server_text(
    text: &str,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<bool, String> {
    match serde_json::from_str::<protocol::ServerEnvelope>(text)
        .map_err(|err| format!("decode server envelope: {err}"))?
    {
        protocol::ServerEnvelope::Event { event } => {
            // RuntimeEvent 保留 payload 兼容层，TUI 当前仍按历史 RuntimeToUiEvent 解码。
            let event = serde_json::from_value::<RuntimeToUiEvent>(event.payload)
                .map_err(|err| format!("decode runtime event: {err}"))?;
            event_tx
                .send(event)
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(false)
        }
        protocol::ServerEnvelope::RuntimeStatus { status } => {
            event_tx
                .send(RuntimeToUiEvent::RuntimeStatusSynced { status })
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(true)
        }
        // controller/role envelope 先作为协议能力保留，当前 TUI 渲染还主要依赖 runtime payload。
        protocol::ServerEnvelope::ControllerChanged { .. } => Ok(false),
        protocol::ServerEnvelope::ClientRoleChanged { .. } => Ok(false),
    }
}

async fn fetch_calibrated_runtime_status(
    http: &reqwest::Client,
    base: &str,
) -> Option<protocol::SessionRuntimeStatus> {
    let started_at = Instant::now();
    let response: protocol::SessionRuntimeStatusResponse = timeout(
        Duration::from_secs(2),
        get_json(http, &format!("{base}/status")),
    )
    .await
    .ok()?
    .ok()?;
    Some(apply_runtime_status_latency(
        response.status,
        started_at.elapsed(),
    ))
}

fn apply_runtime_status_latency(
    mut status: protocol::SessionRuntimeStatus,
    latency: Duration,
) -> protocol::SessionRuntimeStatus {
    // server 端 query timer 会扣掉等待工具授权/输入的暂停时间；暂停态不叠加客户端延迟，
    // 否则等待用户期间会被误算为工作耗时。
    if should_apply_runtime_status_latency(&status)
        && let Some(activity) = &mut status.activity
    {
        activity.elapsed_ms = activity
            .elapsed_ms
            .saturating_add(duration_millis_u64(latency));
    }
    status
}

fn should_apply_runtime_status_latency(status: &protocol::SessionRuntimeStatus) -> bool {
    status.activity.is_some()
        && status.pending_pauses.is_empty()
        && matches!(
            status.state,
            protocol::SessionRuntimeState::Thinking
                | protocol::SessionRuntimeState::Working
                | protocol::SessionRuntimeState::Compacting
        )
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Deserialize)]
struct DaemonHint {
    #[serde(default = "default_daemon_host")]
    host: String,
    port: u16,
}

impl DaemonHint {
    fn addr(&self) -> Result<SocketAddr, String> {
        format!("{}:{}", self.host, self.port)
            .parse()
            .map_err(|err| format!("parse daemon address: {err}"))
    }
}

async fn reconnect_latest_daemon(
    http: &reqwest::Client,
    connection: &mut ProjectConnection,
) -> Result<(), String> {
    let addr = discover_healthy_daemon(http).await?;
    // daemon 重启后端口和进程内 client registry 都会变；先重建身份，再 attach 原项目。
    let register: protocol::RegisterClientResponse = post_json_without_client(
        http,
        &format!("http://{addr}/v1/clients"),
        &protocol::RegisterClientRequest {
            kind: Some("tui".to_string()),
        },
    )
    .await?;
    let cwd = connection.attach.cwd.clone();
    let attach: protocol::ProjectAttachResponse = put_json_without_client(
        http,
        &format!("http://{addr}/v1/projects/{}/attach", connection.project_id),
        &protocol::ProjectAttachRequest { cwd },
    )
    .await?;

    connection.addr = addr;
    connection.project_id = attach.project_id.clone();
    connection.client_id = register.client_id;
    connection.attach = attach;
    Ok(())
}

async fn discover_healthy_daemon(http: &reqwest::Client) -> Result<SocketAddr, String> {
    let hint = read_daemon_hint()?;
    let addr = hint.addr()?;
    let url = format!("http://{addr}/v1/health");
    let response: protocol::DaemonHealthResponse =
        timeout(Duration::from_millis(500), get_json(http, &url))
            .await
            .map_err(|_| format!("GET {url}: timed out"))??;
    if response.ok && response.daemon == "omini-server" {
        Ok(addr)
    } else {
        Err(format!("daemon health check failed at {url}"))
    }
}

fn read_daemon_hint() -> Result<DaemonHint, String> {
    let path = daemon_run_dir()?.join("daemon.json");
    let content =
        fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    serde_json::from_str(&content).map_err(|err| format!("decode {}: {err}", path.display()))
}

fn daemon_run_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".omini").join("run"))
        .ok_or_else(|| "cannot find home dir".to_string())
}

fn default_daemon_host() -> String {
    "127.0.0.1".to_string()
}

async fn handle_local_request(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    base: &str,
    client_id: &str,
    request: ClientRequest,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<LocalAction, String> {
    // 返回 Switch/Blank 表示该请求要求外层退出当前 session loop。
    match request {
        ClientRequest::RunSubmitUserInput { input } => {
            post_json::<_, protocol::RunSubmittedResponse>(
                http,
                &format!("{base}/runs"),
                client_id,
                &protocol::SubmitRunRequest {
                    input,
                    mode: protocol::RunInputMode::Submit,
                },
            )
            .await?;
        }
        ClientRequest::RunInterveneInput { input } => {
            post_json::<_, protocol::RunSubmittedResponse>(
                http,
                &format!("{base}/runs"),
                client_id,
                &protocol::SubmitRunRequest {
                    input,
                    mode: protocol::RunInputMode::Intervene,
                },
            )
            .await?;
        }
        ClientRequest::RunCancel => {
            send_empty(
                http,
                Method::POST,
                &format!("{base}/runs/current/cancel"),
                client_id,
            )
            .await?;
        }
        ClientRequest::ProfileToggle => {
            send_empty(
                http,
                Method::POST,
                &format!("{base}/profile/toggle"),
                client_id,
            )
            .await?;
        }
        ClientRequest::ProfileSet { profile } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/profile"),
                client_id,
                &protocol::SetActiveProfileRequest { profile },
            )
            .await?;
        }
        ClientRequest::OpenModelPicker => {
            let models: protocol::ModelsResponse =
                get_json(http, &format!("{base}/models")).await?;
            event_tx
                .send(RuntimeToUiEvent::InteractionRequest(
                    event_types_model_selection(models),
                ))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
        }
        ClientRequest::ModelSelect {
            provider,
            model,
            thinking_effort,
        } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/model"),
                client_id,
                &protocol::SetModelRequest {
                    provider,
                    model,
                    thinking_effort,
                },
            )
            .await?;
        }
        ClientRequest::ModelThinkingEffortSet { effort } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/thinking-effort"),
                client_id,
                &protocol::SetThinkingEffortRequest { effort },
            )
            .await?;
        }
        ClientRequest::OpenSessionPicker => {
            let sessions: protocol::SessionsResponse =
                get_json(http, &project_sessions_url(connection)).await?;
            event_tx
                .send(RuntimeToUiEvent::InteractionRequest(
                    event_types_session_selection(sessions),
                ))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
        }
        ClientRequest::SessionOpen { session_id } => {
            return Ok(LocalAction::Switch(session_id));
        }
        ClientRequest::SessionNew { profile } => {
            emit_blank_session(event_tx, connection).await?;
            event_tx
                .send(RuntimeToUiEvent::ActiveProfileChanged(profile))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            return Ok(LocalAction::Blank(profile));
        }
        ClientRequest::SessionRename { title } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/rename"),
                client_id,
                &protocol::RenameSessionRequest { title },
            )
            .await?;
        }
        ClientRequest::ContextCompact { instructions } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/compact"),
                client_id,
                &protocol::CompactContextRequest { instructions },
            )
            .await?;
        }
        ClientRequest::ToolPauseResolve {
            tool_use_id,
            response,
        } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/tool-pauses/{tool_use_id}/resolve"),
                client_id,
                &protocol::ResolveToolPauseRequest { response },
            )
            .await?;
        }
        ClientRequest::PlanResolve { plan_id, action } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/plans/{plan_id}/resolve"),
                client_id,
                &protocol::ResolvePlanRequest { action },
            )
            .await?;
        }
        ClientRequest::OpenAgentManager => {
            let agents: protocol::AgentsResponse =
                get_json(http, &format!("{base}/agents")).await?;
            event_tx
                .send(RuntimeToUiEvent::InteractionRequest(
                    event_types_agent_management(agents),
                ))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
        }
        ClientRequest::AgentSave {
            source_kind,
            original_path,
            draft,
        } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/agents"),
                client_id,
                &protocol::SaveAgentRequest {
                    source_kind,
                    original_agent_id: original_path.map(|path| path.display().to_string()),
                    draft,
                },
            )
            .await?;
        }
        ClientRequest::AgentDelete { path } => {
            send_empty(
                http,
                Method::DELETE,
                &format!(
                    "{base}/agents/{}",
                    percent_encode(&path.display().to_string())
                ),
                client_id,
            )
            .await?;
        }
        ClientRequest::AgentGenerate {
            source_kind,
            description,
            tools,
            disallow_tools,
            model,
        } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/agents/generate"),
                client_id,
                &protocol::GenerateAgentRequest {
                    source_kind,
                    description,
                    tools,
                    disallow_tools,
                    model,
                },
            )
            .await?;
        }
        ClientRequest::ExpandSkillRun {
            skill_name,
            prompt,
            input,
        } => {
            let skill: protocol::SkillResponse =
                get_json(http, &format!("{base}/skills/{skill_name}")).await?;
            let run_input = expanded_skill_input(skill.skill, prompt, input);
            post_json::<_, protocol::RunSubmittedResponse>(
                http,
                &format!("{base}/runs"),
                client_id,
                &protocol::SubmitRunRequest {
                    input: run_input,
                    mode: protocol::RunInputMode::Submit,
                },
            )
            .await?;
        }
        ClientRequest::ThinkingDisplaySet { show } => {
            post_json::<_, protocol::AckResponse>(
                http,
                &format!("{base}/thinking-display"),
                client_id,
                &protocol::SetThinkingDisplayRequest { show },
            )
            .await?;
        }
        ClientRequest::AppShutdown => {
            event_tx
                .send(RuntimeToUiEvent::Shutdown)
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
        }
    }
    Ok(LocalAction::None)
}

fn project_sessions_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/sessions",
        connection.addr, connection.project_id
    )
}

fn project_models_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/models",
        connection.addr, connection.project_id
    )
}

fn project_model_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/model",
        connection.addr, connection.project_id
    )
}

fn project_thinking_effort_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/thinking-effort",
        connection.addr, connection.project_id
    )
}

fn project_thinking_display_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/thinking-display",
        connection.addr, connection.project_id
    )
}

fn session_base_url(connection: &ProjectConnection, session_id: &str) -> String {
    format!("{}/{}", project_sessions_url(connection), session_id)
}

async fn get_json<T>(http: &reqwest::Client, url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|err| format!("GET {url}: {err}"))?;
    decode_response(response, url).await
}

async fn post_json<B, T>(
    http: &reqwest::Client,
    url: &str,
    client_id: &str,
    body: &B,
) -> Result<T, String>
where
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
{
    let response = http
        .post(url)
        .header(CLIENT_ID_HEADER, client_id)
        .json(body)
        .send()
        .await
        .map_err(|err| format!("POST {url}: {err}"))?;
    decode_response(response, url).await
}

async fn post_json_without_client<B, T>(
    http: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<T, String>
where
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
{
    let response = http
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|err| format!("POST {url}: {err}"))?;
    decode_response(response, url).await
}

async fn put_json_without_client<B, T>(
    http: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<T, String>
where
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
{
    let response = http
        .put(url)
        .json(body)
        .send()
        .await
        .map_err(|err| format!("PUT {url}: {err}"))?;
    decode_response(response, url).await
}

async fn send_empty(
    http: &reqwest::Client,
    method: Method,
    url: &str,
    client_id: &str,
) -> Result<(), String> {
    let response = http
        .request(method, url)
        .header(CLIENT_ID_HEADER, client_id)
        .send()
        .await
        .map_err(|err| format!("request {url}: {err}"))?;
    let _: protocol::AckResponse = decode_response(response, url).await?;
    Ok(())
}

async fn decode_response<T>(response: reqwest::Response, url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .await
            .map_err(|err| format!("decode response {url}: {err}"));
    }
    let text = response.text().await.unwrap_or_default();
    // server 错误优先按协议错误显示给用户，保留原始响应只作为兜底诊断信息。
    if let Ok(error) = serde_json::from_str::<ProtocolError>(&text) {
        Err(error.message)
    } else {
        Err(format!("{url} returned {status}: {text}"))
    }
}

pub(crate) fn thinking_effort_from_protocol(
    effort: protocol::ThinkingEffort,
) -> crate::types::config::ThinkingEffort {
    match effort {
        protocol::ThinkingEffort::None => crate::types::config::ThinkingEffort::None,
        protocol::ThinkingEffort::Low => crate::types::config::ThinkingEffort::Low,
        protocol::ThinkingEffort::Medium => crate::types::config::ThinkingEffort::Medium,
        protocol::ThinkingEffort::High => crate::types::config::ThinkingEffort::High,
    }
}

pub(crate) fn session_summary_from_protocol(
    session: protocol::SessionSummary,
) -> crate::types::events::SessionSummary {
    crate::types::events::SessionSummary {
        id: session.id,
        title: session.title,
        model: session.model,
        provider: session.provider,
        created_at: session.created_at,
        updated_at: session.updated_at,
        runtime_state: session.runtime_state,
    }
}

pub(crate) fn agent_summary_from_protocol(
    agent: protocol::AgentSummary,
) -> crate::subagents::AgentSummary {
    crate::subagents::AgentSummary {
        name: agent.name,
        description: agent.description,
    }
}

pub(crate) fn skill_command_summary(
    skill: protocol::SkillSummary,
) -> crate::types::events::CommandSummary {
    crate::types::events::CommandSummary {
        name: skill.name,
        aliases: Vec::new(),
        description: skill.description,
        sort_weight: 500,
        kind: crate::types::events::CommandKind::Skill,
        has_args: true,
        args_description: Some("[prompt]".to_string()),
    }
}

fn event_types_session_selection(
    sessions: protocol::SessionsResponse,
) -> crate::types::events::InteractionRequest {
    crate::types::events::InteractionRequest::SessionSelection {
        sessions: sessions
            .sessions
            .into_iter()
            .map(session_summary_from_protocol)
            .collect(),
    }
}

fn event_types_model_selection(
    models: protocol::ModelsResponse,
) -> crate::types::events::InteractionRequest {
    crate::types::events::InteractionRequest::ModelSelection {
        providers: providers_from_protocol(models.providers),
        current_provider: models.current_provider,
        current_model: models.current_model,
    }
}

fn event_types_agent_management(
    agents: protocol::AgentsResponse,
) -> crate::types::events::InteractionRequest {
    crate::types::events::InteractionRequest::AgentManagement {
        records: agents
            .records
            .into_iter()
            .map(agent_record_from_protocol)
            .collect(),
        providers: providers_from_protocol(agents.providers),
        current_provider: agents.current_provider,
        current_model: agents.current_model,
    }
}

fn providers_from_protocol(
    providers: Vec<protocol::ProviderInfo>,
) -> std::collections::HashMap<String, crate::types::config::ProviderProfile> {
    providers
        .into_iter()
        .map(|provider| {
            (
                provider.id,
                crate::types::config::ProviderProfile {
                    name: provider.name,
                    endpoint: provider_endpoint_from_protocol(provider.endpoint),
                    base_url: provider.base_url,
                    models: provider
                        .models
                        .into_iter()
                        .map(model_config_from_protocol)
                        .collect(),
                },
            )
        })
        .collect()
}

fn provider_endpoint_from_protocol(
    endpoint: protocol::ProviderEndpointKind,
) -> crate::types::config::ProviderType {
    match endpoint {
        protocol::ProviderEndpointKind::OpenAI => crate::types::config::ProviderType::OpenAI,
        protocol::ProviderEndpointKind::Anthropic => crate::types::config::ProviderType::Anthropic,
    }
}

fn model_config_from_protocol(model: protocol::ModelInfo) -> crate::types::config::ModelConfig {
    crate::types::config::ModelConfig {
        id: model.id,
        name: model.name,
        limit: model.limit,
        thinking: model.thinking,
        input_modalities: model.input_modalities.map(|modalities| {
            modalities
                .into_iter()
                .map(input_modality_from_protocol)
                .collect()
        }),
    }
}

fn input_modality_from_protocol(
    modality: protocol::InputModality,
) -> crate::types::config::InputModality {
    match modality {
        protocol::InputModality::Text => crate::types::config::InputModality::Text,
        protocol::InputModality::Image => crate::types::config::InputModality::Image,
    }
}

fn agent_record_from_protocol(record: protocol::AgentRecord) -> crate::subagents::AgentRecord {
    crate::subagents::AgentRecord {
        name: record.name,
        description: record.description,
        instructions: record.instructions,
        tools: record.tools,
        disallow_tools: record.disallow_tools,
        model: record.model,
        source_kind: agent_source_kind_from_protocol(record.source_kind),
        path: record.editable.then(|| PathBuf::from(record.id)),
        editable: record.editable,
    }
}

fn agent_source_kind_from_protocol(
    source_kind: protocol::AgentSourceKind,
) -> crate::subagents::AgentSourceKind {
    match source_kind {
        protocol::AgentSourceKind::BuiltIn => crate::subagents::AgentSourceKind::BuiltIn,
        protocol::AgentSourceKind::Project => crate::subagents::AgentSourceKind::Project,
        protocol::AgentSourceKind::User => crate::subagents::AgentSourceKind::User,
    }
}

fn expanded_skill_input(
    skill: protocol::SkillDetail,
    prompt: String,
    input: Option<protocol::UserInput>,
) -> protocol::UserInput {
    // slash skill 在 TUI 侧展开成普通用户输入，server/core 不需要知道本地命令语法。
    let mut text = render_skill_slash_command_invocation(&skill, Some(&prompt));
    if let Some(context) = input
        .as_ref()
        .and_then(|input| context_text(input.context_refs.as_deref()))
    {
        text.push_str("\n\n");
        text.push_str(&context);
    }
    protocol::UserInput {
        text,
        context_refs: None,
        attachments: input.and_then(|input| input.attachments),
    }
}

fn render_skill_slash_command_invocation(
    skill: &protocol::SkillDetail,
    prompt: Option<&str>,
) -> String {
    let mut output = String::new();
    output.push_str("<skill>\n");
    output.push_str("<skill_name>");
    output.push_str(&skill.name);
    output.push_str("</skill_name>\n");
    output.push_str("<skill_invocation>\n");
    output.push_str("<source>slash_command</source>\n");
    output.push_str("</skill_invocation>\n");
    output.push_str("<skill_directory>");
    output.push_str(&skill.directory);
    output.push_str("</skill_directory>\n");
    output.push_str("<skill_body>\n");
    output.push_str(skill.body.trim());
    output.push_str("\n</skill_body>");
    if let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) {
        output.push_str("\n<user_prompt>\n");
        output.push_str(prompt);
        output.push_str("\n</user_prompt>");
    }
    output.push_str("\n</skill>");
    output
}

fn context_text(context_refs: Option<&[protocol::ContextRef]>) -> Option<String> {
    let refs = context_refs?;
    if refs.is_empty() {
        return None;
    }
    let mut output = String::from("Referenced context:\n");
    for context_ref in refs {
        output.push_str("- ");
        output.push_str(&context_ref_text(context_ref));
        output.push('\n');
    }
    Some(output)
}

fn context_ref_text(context_ref: &protocol::ContextRef) -> String {
    match context_ref {
        protocol::ContextRef::File { path, label } => format!(
            "File: @{}. Read this file if needed.",
            label.as_deref().unwrap_or(path)
        ),
        protocol::ContextRef::Directory { path, label } => format!(
            "Directory: @{}. Inspect this directory if needed.",
            label.as_deref().unwrap_or(path)
        ),
        protocol::ContextRef::Subagent { name, label } => format!(
            "Agent: @{}. Use subagent \"{}\" if this helps answer the user.",
            label.as_deref().unwrap_or(name),
            name
        ),
        protocol::ContextRef::Url { url, label } => {
            format!(
                "URL: @{}. Use this URL if needed.",
                label.as_deref().unwrap_or(url)
            )
        }
    }
}

fn percent_encode(value: &str) -> String {
    // agent id 目前作为 path segment 传输，手写最小 percent-encode 避免引入额外依赖。
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn query_runtime_status() -> protocol::SessionRuntimeStatus {
        protocol::SessionRuntimeStatus {
            session_id: "session_1".to_string(),
            state: protocol::SessionRuntimeState::Working,
            active_profile: protocol::ActiveProfile::Main,
            loaded: true,
            controller_id: Some("client_1".to_string()),
            connected_client_count: 1,
            activity: Some(protocol::SessionRuntimeActivity {
                kind: protocol::SessionRuntimeActivityKind::Query,
                started_at: Utc::now(),
                elapsed_ms: 1_500,
            }),
            pending_pauses: Vec::new(),
            pending_plan_approval: None,
            active_tools: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            subagents: Vec::new(),
        }
    }

    fn envelope_text(event: RuntimeToUiEvent) -> String {
        let payload = serde_json::to_value(event).expect("event should serialize");
        let kind = payload
            .get("type")
            .and_then(serde_json::Value::as_str)
            .expect("event should have type")
            .to_string();
        serde_json::to_string(&protocol::ServerEnvelope::Event {
            event: protocol::RuntimeEvent::new(kind, payload),
        })
        .expect("envelope should serialize")
    }

    fn runtime_status_envelope_text(status: protocol::SessionRuntimeStatus) -> String {
        serde_json::to_string(&protocol::ServerEnvelope::RuntimeStatus { status })
            .expect("envelope should serialize")
    }

    fn pending_pause() -> protocol::SessionRuntimePendingPause {
        protocol::SessionRuntimePendingPause {
            tool_use_id: "tool_1".to_string(),
            tool_name: "bash".to_string(),
            kind: protocol::ToolPauseEventKind::Permission,
            source_session_id: None,
            source_agent_label: None,
        }
    }

    #[test]
    fn session_summary_mapping_preserves_runtime_state() {
        let now = Utc::now();
        let summary = session_summary_from_protocol(protocol::SessionSummary {
            id: "session_1".to_string(),
            title: "hello".to_string(),
            model: "gpt-test".to_string(),
            provider: "openai".to_string(),
            created_at: now,
            updated_at: now,
            runtime_state: Some(protocol::SessionRuntimeState::Compacting),
        });

        assert_eq!(
            summary.runtime_state,
            Some(protocol::SessionRuntimeState::Compacting)
        );
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
        status.state = protocol::SessionRuntimeState::Compacting;
        status.activity = Some(protocol::SessionRuntimeActivity {
            kind: protocol::SessionRuntimeActivityKind::Compact,
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
        waiting.state = protocol::SessionRuntimeState::Waiting;
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

        let saw_runtime_status =
            handle_server_text(&runtime_status_envelope_text(query_runtime_status()), &tx)
                .await
                .expect("runtime status should decode");

        assert!(saw_runtime_status);
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeToUiEvent::RuntimeStatusSynced { status })
                if status.session_id == "session_1" && status.activity.is_some()
        ));
    }

    #[tokio::test]
    async fn handle_server_text_still_decodes_legacy_runtime_event() {
        let (tx, mut rx) = mpsc::channel(4);

        let saw_runtime_status = handle_server_text(
            &envelope_text(RuntimeToUiEvent::SessionChanged {
                session_id: Some("session_1".to_string()),
                messages: Vec::new(),
                subagents: Vec::new(),
                usage: Default::default(),
            }),
            &tx,
        )
        .await
        .expect("session changed should decode");

        assert!(!saw_runtime_status);
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeToUiEvent::SessionChanged { .. })
        ));
    }
}
