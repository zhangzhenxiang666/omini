use crate::app::event::{RuntimeToUiEvent, ThreadUsageSnapshot};
use omini_protocol as protocol;
use reqwest::Method;
use std::collections::VecDeque;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::client::*;
pub async fn handle_project_request(
    http: &reqwest::Client,
    connection: &mut ProjectConnection,
    request: ClientRequest,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    blank_profile: &mut protocol::ActiveProfile,
) -> Result<ProjectAction, String> {
    match request {
        ClientRequest::RunSubmitUserInput { .. } | ClientRequest::RunExecuteCommand { .. } => {
            let thread_id = create_thread(http, connection, *blank_profile, event_tx).await?;
            let mut pending = VecDeque::new();
            pending.push_back(request);
            Ok(ProjectAction::Connect { thread_id, pending })
        }
        ClientRequest::OpenThreadPicker => {
            let threads: protocol::ThreadsResponse =
                get_json(http, &project_threads_url(connection)).await?;
            event_tx
                .send(RuntimeToUiEvent::InteractionRequest(
                    event_types_thread_selection(threads),
                ))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            Ok(ProjectAction::None)
        }
        ClientRequest::ThreadOpen { thread_id } => Ok(ProjectAction::Connect {
            thread_id,
            pending: VecDeque::new(),
        }),
        ClientRequest::ThreadNew { profile } => {
            *blank_profile = profile;
            emit_blank_thread(event_tx, connection).await?;
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
        ClientRequest::OpenAgentManager => {
            open_agent_manager(http, connection, event_tx).await?;
            Ok(ProjectAction::None)
        }
        ClientRequest::AgentSave {
            source_kind,
            original_path,
            draft,
        } => {
            save_agent(http, connection, None, source_kind, original_path, draft).await?;
            refresh_agent_management_panel(http, connection, event_tx).await?;
            Ok(ProjectAction::None)
        }
        ClientRequest::AgentDelete { path } => {
            delete_agent(http, connection, None, path).await?;
            refresh_agent_management_panel(http, connection, event_tx).await?;
            Ok(ProjectAction::None)
        }
        ClientRequest::AgentGenerate {
            source_kind,
            description,
            tools,
            disallow_tools,
            agent_model,
            provider,
            model,
            thinking_effort,
        } => {
            generate_agent(
                http,
                connection,
                event_tx,
                AgentGenerateRequest {
                    source_kind,
                    description,
                    tools,
                    disallow_tools,
                    agent_model,
                    provider,
                    model,
                    thinking_effort,
                },
            )
            .await?;
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

pub async fn create_thread(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    profile: protocol::ActiveProfile,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<String, String> {
    let response: protocol::CreateThreadResponse = post_json_without_client(
        http,
        &project_threads_url(connection),
        &protocol::CreateThreadRequest {
            provider: Some(connection.open.active_provider.clone()),
            model: Some(connection.open.model.clone()),
            thinking_effort: connection.open.thinking_effort,
            profile: Some(profile),
        },
    )
    .await?;
    let thread_id = response.thread_id;
    event_tx
        .send(RuntimeToUiEvent::ActiveProfileChanged(profile))
        .await
        .map_err(|_| "TUI event receiver closed".to_string())?;
    Ok(thread_id)
}

pub async fn emit_blank_thread(
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    connection: &ProjectConnection,
) -> Result<(), String> {
    event_tx
        .send(RuntimeToUiEvent::ThreadSnapshot {
            thread_id: None,
            messages: Vec::new(),
            agent_tasks: Vec::new(),
            usage: ThreadUsageSnapshot {
                context_window: connection.open.context_window,
                ..ThreadUsageSnapshot::default()
            },
        })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())
}

pub async fn apply_project_runtime_config(
    connection: &mut ProjectConnection,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    config: protocol::ProjectRuntimeConfigResponse,
) -> Result<(), String> {
    connection.open.active_provider = config.active_provider.clone();
    connection.open.model = config.model.clone();
    connection.open.thinking_effort = config.thinking_effort;
    connection.open.context_window = config.context_window;

    event_tx
        .send(RuntimeToUiEvent::ModelChanged {
            provider: config.active_provider,
            model: config.model,
            thinking_effort: config.thinking_effort.map(thinking_effort_from_protocol),
            context_window: config.context_window,
        })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())
}

pub struct AgentGenerateRequest {
    pub source_kind: protocol::AgentSourceKind,
    pub description: String,
    pub tools: Vec<String>,
    pub disallow_tools: Vec<String>,
    pub agent_model: Option<String>,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<protocol::ThinkingEffort>,
}

pub async fn open_agent_manager(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<(), String> {
    let agents: protocol::AgentsResponse = get_json(http, &project_agents_url(connection)).await?;
    event_tx
        .send(RuntimeToUiEvent::InteractionRequest(
            event_types_agent_management(agents),
        ))
        .await
        .map_err(|_| "TUI event receiver closed".to_string())
}

pub async fn refresh_agent_management_panel(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<(), String> {
    let agents: protocol::AgentsResponse = get_json(http, &project_agents_url(connection)).await?;
    event_tx
        .send(RuntimeToUiEvent::AgentManagementUpdated {
            records: agent_records_from_protocol(agents.records),
        })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())
}

pub async fn save_agent(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    target_thread_id: Option<&str>,
    source_kind: protocol::AgentSourceKind,
    original_path: Option<PathBuf>,
    draft: protocol::AgentDraft,
) -> Result<(), String> {
    let _: protocol::AckResponse = post_json_without_client(
        http,
        &project_agents_url_with_target(connection, target_thread_id),
        &protocol::SaveAgentRequest {
            source_kind,
            original_agent_id: original_path.map(|path| path.display().to_string()),
            draft,
        },
    )
    .await?;
    Ok(())
}

pub async fn delete_agent(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    target_thread_id: Option<&str>,
    path: PathBuf,
) -> Result<(), String> {
    send_empty_without_client(
        http,
        Method::DELETE,
        &project_agent_url(connection, &path.display().to_string(), target_thread_id),
    )
    .await
}

pub async fn generate_agent(
    http: &reqwest::Client,
    connection: &ProjectConnection,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    request: AgentGenerateRequest,
) -> Result<(), String> {
    let response: protocol::GenerateAgentResponse = post_json_without_client(
        http,
        &project_agent_generate_url(connection),
        &protocol::GenerateAgentRequest {
            description: request.description,
            provider: request.provider,
            model: request.model,
            thinking_effort: request.thinking_effort,
        },
    )
    .await?;
    event_tx
        .send(RuntimeToUiEvent::AgentGenerated {
            source_kind: agent_source_kind_from_protocol(request.source_kind),
            draft: generated_agent_draft_from_protocol(
                response.draft,
                request.tools,
                request.disallow_tools,
                request.agent_model,
            ),
        })
        .await
        .map_err(|_| "TUI event receiver closed".to_string())
}

pub fn toggle_profile(profile: protocol::ActiveProfile) -> protocol::ActiveProfile {
    match profile {
        protocol::ActiveProfile::Main => protocol::ActiveProfile::Auto,
        protocol::ActiveProfile::Auto => protocol::ActiveProfile::Plan,
        protocol::ActiveProfile::Plan => protocol::ActiveProfile::Main,
    }
}

pub fn request_name(request: &ClientRequest) -> &'static str {
    match request {
        ClientRequest::RunSubmitUserInput { .. } => "submit",
        ClientRequest::RunInterveneInput { .. } => "intervene",
        ClientRequest::AgentTaskSubmitInput { .. } => "agent_task_input",
        ClientRequest::AgentTaskCancel { .. } => "agent_task_cancel",
        ClientRequest::RunExecuteCommand { .. } => "command",
        ClientRequest::RunCancel => "cancel",
        ClientRequest::ProfileToggle => "profile toggle",
        ClientRequest::ProfileSet { .. } => "profile",
        ClientRequest::OpenModelPicker => "model picker",
        ClientRequest::ModelSelect { .. } => "model select",
        ClientRequest::ModelThinkingEffortSet { .. } => "thinking effort",
        ClientRequest::OpenThreadPicker => "sessions",
        ClientRequest::ThreadOpen { .. } => "session open",
        ClientRequest::ThreadNew { .. } => "new session",
        ClientRequest::ThreadRename { .. } => "rename",
        ClientRequest::ContextCompact { .. } => "compact",
        ClientRequest::ToolPauseResolve { .. } => "tool pause",
        ClientRequest::PlanResolve { .. } => "plan approval",
        ClientRequest::OpenAgentManager => "agents",
        ClientRequest::AgentSave { .. } => "agent save",
        ClientRequest::AgentDelete { .. } => "agent delete",
        ClientRequest::AgentGenerate { .. } => "agent generate",
        ClientRequest::AppShutdown => "shutdown",
    }
}
