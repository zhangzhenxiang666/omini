use crate::types::events::RuntimeToUiEvent;
use futures_util::{SinkExt, StreamExt};
use omini_protocol as protocol;
use omini_protocol::ProtocolError;
use reqwest::Method;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
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
    SessionNew,
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
    connection: ProjectConnection,
    event_tx: mpsc::Sender<RuntimeToUiEvent>,
    mut request_rx: mpsc::Receiver<ClientRequest>,
) -> Result<(), String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|err| format!("build HTTP client: {err}"))?;
    let mut active_session_id: Option<String> = None;
    let mut pending_request: Option<ClientRequest> = None;

    loop {
        if let Some(session_id) = active_session_id.take() {
            // 一次只维护一个活跃 session WebSocket；切换会话时结束旧 loop 再连接新 loop。
            match run_connected_session(
                &http,
                &connection,
                &session_id,
                &event_tx,
                &mut request_rx,
                pending_request.take(),
            )
            .await
            {
                Ok(SessionLoop::Switch(next_session_id)) => {
                    active_session_id = Some(next_session_id);
                }
                Ok(SessionLoop::Closed) => {}
                Err(err) => {
                    let _ = event_tx.send(RuntimeToUiEvent::error(err)).await;
                }
            }
            continue;
        }

        let Some(request) = request_rx.recv().await else {
            break;
        };
        // 没有活跃 session 时，项目级请求可以直接处理；会话级请求会先创建 session 再补发。
        match handle_project_request(&http, &connection, request, &event_tx).await? {
            ProjectAction::None => {}
            ProjectAction::Connect {
                session_id,
                pending,
            } => {
                active_session_id = Some(session_id);
                pending_request = pending;
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
        pending: Option<ClientRequest>,
    },
    Shutdown,
}

enum SessionLoop {
    Switch(String),
    Closed,
}

async fn handle_project_request(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    request: ClientRequest,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<ProjectAction, String> {
    match request {
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
            pending: None,
        }),
        ClientRequest::SessionNew => {
            let session_id = create_session(http, connection).await?;
            Ok(ProjectAction::Connect {
                session_id,
                pending: None,
            })
        }
        ClientRequest::AppShutdown => {
            event_tx
                .send(RuntimeToUiEvent::Shutdown)
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::Shutdown)
        }
        other => {
            let session_id = create_session(http, connection).await?;
            // 用户在无活跃会话时触发会话内动作，先创建会话，再交给 session loop 处理原动作。
            Ok(ProjectAction::Connect {
                session_id,
                pending: Some(other),
            })
        }
    }
}

async fn create_session(
    http: &reqwest::Client,
    connection: &ProjectConnection,
) -> Result<String, String> {
    let response: protocol::CreateSessionResponse =
        post_json_without_client(http, &project_sessions_url(connection), &()).await?;
    response
        .session_id
        .ok_or_else(|| "Server did not return a session id".to_string())
}

async fn run_connected_session(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    session_id: &str,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    request_rx: &mut mpsc::Receiver<ClientRequest>,
    initial_request: Option<ClientRequest>,
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
    let mut pending_status_sync = fetch_query_runtime_status_sync(http, &base).await;

    if let Some(request) = initial_request
        && let Some(next_session_id) =
            handle_local_request(http, connection, &base, &client_id, request, event_tx).await?
    {
        // pending request 也可能是打开/新建会话，出现时直接切到目标 session。
        return Ok(SessionLoop::Switch(next_session_id));
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
                    Ok(Some(next_session_id)) => {
                        return Ok(SessionLoop::Switch(next_session_id));
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = event_tx.send(RuntimeToUiEvent::error(err)).await;
                    }
                }
            }
            // WebSocket 流只向 UI 注入 server 事件，连接控制帧在这里就地处理。
            message = read.next() => {
                let Some(message) = message else {
                    break;
                };
                let message = message.map_err(|err| format!("read server message: {err}"))?;
                match message {
                    TungsteniteMessage::Text(text) => {
                        handle_server_text(text.as_str(), event_tx, &mut pending_status_sync)
                            .await?;
                    }
                    TungsteniteMessage::Close(_) => break,
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

    Ok(SessionLoop::Closed)
}

async fn fetch_query_runtime_status_sync(
    http: &reqwest::Client,
    base: &str,
) -> Option<protocol::SessionRuntimeStatus> {
    let response: protocol::SessionRuntimeStatusResponse = timeout(
        Duration::from_secs(2),
        get_json(http, &format!("{base}/status")),
    )
    .await
    .ok()?
    .ok()?;
    let status = response.status;
    let is_query = status
        .activity
        .as_ref()
        .is_some_and(|activity| activity.kind == protocol::SessionRuntimeActivityKind::Query);
    is_query.then_some(status)
}

async fn handle_server_text(
    text: &str,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    pending_status_sync: &mut Option<protocol::SessionRuntimeStatus>,
) -> Result<(), String> {
    match serde_json::from_str::<protocol::ServerEnvelope>(text)
        .map_err(|err| format!("decode server envelope: {err}"))?
    {
        protocol::ServerEnvelope::Event { event } => {
            // RuntimeEvent 保留 payload 兼容层，TUI 当前仍按历史 RuntimeToUiEvent 解码。
            let event = serde_json::from_value::<RuntimeToUiEvent>(event.payload)
                .map_err(|err| format!("decode runtime event: {err}"))?;
            let should_sync_status = matches!(event, RuntimeToUiEvent::RunStarted);
            let should_drop_status =
                should_drop_pending_status_sync(&event, pending_status_sync.as_ref());
            event_tx
                .send(event)
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            if should_sync_status {
                if let Some(status) = pending_status_sync.take() {
                    event_tx
                        .send(RuntimeToUiEvent::RuntimeStatusSynced { status })
                        .await
                        .map_err(|_| "TUI event receiver closed".to_string())?;
                }
            } else if should_drop_status {
                pending_status_sync.take();
            }
            Ok(())
        }
        // controller/role envelope 先作为协议能力保留，当前 TUI 渲染还主要依赖 runtime payload。
        protocol::ServerEnvelope::ControllerChanged { .. } => Ok(()),
        protocol::ServerEnvelope::ClientRoleChanged { .. } => Ok(()),
    }
}

fn should_drop_pending_status_sync(
    event: &RuntimeToUiEvent,
    pending_status_sync: Option<&protocol::SessionRuntimeStatus>,
) -> bool {
    match event {
        RuntimeToUiEvent::RunFinished => true,
        RuntimeToUiEvent::SessionChanged { session_id, .. } => pending_status_sync
            .is_some_and(|status| session_id.as_deref() != Some(status.session_id.as_str())),
        _ => false,
    }
}

async fn handle_local_request(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    base: &str,
    client_id: &str,
    request: ClientRequest,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<Option<String>, String> {
    // 返回 Some(session_id) 表示该请求不是普通 mutation，而是要求外层切换 session loop。
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
            return Ok(Some(session_id));
        }
        ClientRequest::SessionNew => {
            return Ok(Some(create_session(http, connection).await?));
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
    Ok(None)
}

fn project_sessions_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/sessions",
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
            loaded: true,
            controller_id: Some("client_1".to_string()),
            connected_client_count: 1,
            activity: Some(protocol::SessionRuntimeActivity {
                kind: protocol::SessionRuntimeActivityKind::Query,
                started_at: Utc::now(),
                elapsed_ms: 1_500,
            }),
            pending_pauses: Vec::new(),
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

    #[tokio::test]
    async fn handle_server_text_emits_status_sync_after_run_started() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut pending_status_sync = Some(query_runtime_status());

        handle_server_text(
            &envelope_text(RuntimeToUiEvent::SessionChanged {
                session_id: Some("session_1".to_string()),
                messages: Vec::new(),
                subagents: Vec::new(),
                usage: Default::default(),
            }),
            &tx,
            &mut pending_status_sync,
        )
        .await
        .expect("session changed should decode");

        assert!(pending_status_sync.is_some());
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeToUiEvent::SessionChanged { .. })
        ));

        handle_server_text(
            &envelope_text(RuntimeToUiEvent::RunStarted),
            &tx,
            &mut pending_status_sync,
        )
        .await
        .expect("run started should decode");

        assert!(pending_status_sync.is_none());
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeToUiEvent::RunStarted)
        ));
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeToUiEvent::RuntimeStatusSynced { status })
                if status.session_id == "session_1" && status.activity.is_some()
        ));
    }
}
