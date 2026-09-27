use crate::app::event::RuntimeToUiEvent;
use omini_protocol as protocol;
use reqwest::Method;
use tokio::sync::mpsc;

use crate::client::*;
pub async fn handle_local_request(
    http: &reqwest::Client,
    connection: &mut ProjectConnection,
    thread_id: &str,
    base: &str,
    client_id: &str,
    request: ClientRequest,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
) -> Result<LocalAction, String> {
    // 返回 Switch/Blank 表示该请求要求外层退出当前 thread loop。
    match request {
        ClientRequest::RunSubmitUserInput {
            mut input,
            client_echo_id,
        } => {
            upload_images(http, base, client_id, &mut input).await?;
            post_json::<_, protocol::RunSubmittedResponse>(
                http,
                &format!("{base}/runs"),
                client_id,
                &protocol::SubmitRunRequest::SubmitMessage {
                    input: input.input,
                    client_echo_id,
                },
            )
            .await?;
        }
        ClientRequest::RunInterveneInput {
            mut input,
            client_echo_id,
        } => {
            upload_images(http, base, client_id, &mut input).await?;
            post_json::<_, protocol::RunSubmittedResponse>(
                http,
                &format!("{base}/runs"),
                client_id,
                &protocol::SubmitRunRequest::InterveneMessage {
                    input: input.input,
                    client_echo_id,
                },
            )
            .await?;
        }
        ClientRequest::AgentTaskSubmitInput {
            task_id,
            mut input,
            client_echo_id,
        } => {
            upload_images(http, base, client_id, &mut input).await?;
            post_no_content(
                http,
                &format!("{base}/runs/{task_id}/input"),
                client_id,
                &protocol::AgentRunInputRequest {
                    input: input.input,
                    client_echo_id: client_echo_id
                        .ok_or_else(|| "child input requires client_echo_id".to_string())?,
                },
            )
            .await?;
        }
        ClientRequest::AgentTaskCancel { task_id } => {
            send_empty(
                http,
                Method::POST,
                &format!("{base}/runs/{task_id}/cancel"),
                client_id,
            )
            .await?;
        }
        ClientRequest::RunExecuteCommand {
            command,
            mut input,
            client_echo_id,
        } => {
            upload_images(http, base, client_id, &mut input).await?;
            post_json::<_, protocol::RunSubmittedResponse>(
                http,
                &format!("{base}/runs"),
                client_id,
                &protocol::SubmitRunRequest::ExecuteCommand {
                    command,
                    input: input.input,
                    client_echo_id,
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
            post_no_content(
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
            // 1. 更新当前 thread runtime
            post_no_content(
                http,
                &format!("{base}/model"),
                client_id,
                &protocol::SetModelRequest {
                    provider: provider.clone(),
                    model: model.clone(),
                    thinking_effort,
                },
            )
            .await?;

            // 2. 同步更新本地缓存的运行时配置（不写入 project/state.toml），
            //    确保 /new 或 /clear 后创建的新 thread 沿用本次选择的 model/provider
            //    以及本地已有的 thinking_effort（若本次未指定则保留原值）。
            let config = protocol::ProjectRuntimeConfigResponse {
                active_provider: provider,
                model,
                thinking_effort: thinking_effort.or(connection.open.thinking_effort),
                context_window: connection.open.context_window,
            };
            apply_project_runtime_config(connection, event_tx, config).await?;
        }
        ClientRequest::ModelThinkingEffortSet { effort } => {
            // 1. 更新当前 thread runtime
            post_no_content(
                http,
                &format!("{base}/thinking-effort"),
                client_id,
                &protocol::SetThinkingEffortRequest { effort },
            )
            .await?;

            // 2. 同步更新本地缓存的运行时配置（不写入 project/state.toml），
            //    确保 /new 或 /clear 后创建的新 thread 沿用本次选择的 thinking effort。
            let config = protocol::ProjectRuntimeConfigResponse {
                active_provider: connection.open.active_provider.clone(),
                model: connection.open.model.clone(),
                thinking_effort: Some(effort),
                context_window: connection.open.context_window,
            };
            apply_project_runtime_config(connection, event_tx, config).await?;
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
        }
        ClientRequest::ThreadOpen { thread_id } => {
            return Ok(LocalAction::Switch(thread_id));
        }
        ClientRequest::ThreadNew { profile } => {
            emit_blank_thread(event_tx, connection).await?;
            event_tx
                .send(RuntimeToUiEvent::ActiveProfileChanged(profile.into()))
                .await
                .map_err(|_| "TUI event receiver closed".to_string())?;
            return Ok(LocalAction::Blank(profile));
        }
        ClientRequest::ThreadRename { title } => {
            post_no_content(
                http,
                &format!("{base}/rename"),
                client_id,
                &protocol::RenameThreadRequest { title },
            )
            .await?;
        }
        ClientRequest::ContextCompact { instructions } => {
            post_no_content(
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
            post_no_content(
                http,
                &format!("{base}/tool-pauses/{tool_use_id}/resolve"),
                client_id,
                &protocol::ResolveToolPauseRequest { response },
            )
            .await?;
        }
        ClientRequest::PlanResolve { action } => {
            post_no_content(
                http,
                &format!("{base}/plans/plan/resolve"),
                client_id,
                &protocol::ResolvePlanRequest { action },
            )
            .await?;
        }
        ClientRequest::OpenAgentManager => {
            open_agent_manager(http, connection, event_tx).await?;
        }
        ClientRequest::AgentSave {
            source_kind,
            original_path,
            draft,
        } => {
            save_agent(
                http,
                connection,
                Some(thread_id),
                source_kind,
                original_path,
                draft,
            )
            .await?;
            refresh_agent_management_panel(http, connection, event_tx).await?;
        }
        ClientRequest::AgentDelete { path } => {
            delete_agent(http, connection, Some(thread_id), path).await?;
            refresh_agent_management_panel(http, connection, event_tx).await?;
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
