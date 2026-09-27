use crate::event::bridge::{
    fallback_thread_title_from_user_input, resolve_plan_command_from_protocol_request,
    resolve_tool_pause_command_from_protocol_request, run_submitted_response_from_runtime_result,
    submit_run_command_from_protocol_request_for_thread,
};
use crate::event::tool_pause::ToolPauseResolutionStart;
use crate::routes::extract::ClientId;
use crate::routes::extract::{ApiJson, ApiPath, ApiQuery};
use crate::routes::{ApiResult, api_error, core_error, require_daemon_thread, require_project};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use omini_protocol as protocol;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Default, Deserialize)]
pub struct ListRunsQuery {
    #[serde(default)]
    include_archived: bool,
}

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new()
        .routes(utoipa_axum::routes!(list_agent_runs))
        .routes(utoipa_axum::routes!(get_agent_run))
        .routes(utoipa_axum::routes!(archive_agent_run))
        .routes(utoipa_axum::routes!(submit_agent_input))
        .routes(utoipa_axum::routes!(submit_run))
        .routes(utoipa_axum::routes!(cancel_run))
        .routes(utoipa_axum::routes!(resolve_tool_pause))
        .routes(utoipa_axum::routes!(resolve_plan))
}

/// 列出线程的 AgentRun，可选择包含已归档记录。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads/{thread_id}/runs",
    operation_id = "list_agent_runs",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("include_archived" = Option<bool>, Query, description = "是否包含已归档运行")),
    responses((status = 200, description = "成功", body = omini_protocol::AgentRunsResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_agent_runs(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    ApiQuery(query): ApiQuery<ListRunsQuery>,
) -> ApiResult<protocol::AgentRunsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .list_agent_runs(&thread_id, query.include_archived)
        .await
        .map(Json)
        .map_err(core_error)
}

/// 读取指定 AgentRun 及其步骤和工具调用详情。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads/{thread_id}/runs/{run_id}",
    operation_id = "get_agent_run",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("run_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::AgentRunDetailResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn get_agent_run(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id, run_id)): ApiPath<(String, String, String)>,
) -> ApiResult<protocol::AgentRunDetailResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .get_agent_run_detail(&thread_id, &run_id)
        .await
        .map_err(core_error)?
        .map(Json)
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "run_not_found",
                "AgentRun does not exist",
            )
        })
}

/// 归档或恢复 AgentRun；运行中的记录不能归档。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/runs/{run_id}/archive",
    operation_id = "archive_agent_run",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("run_id" = String, Path)),
    request_body = omini_protocol::ArchiveAgentRunRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn archive_agent_run(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id, run_id)): ApiPath<(String, String, String)>,
    ApiJson(request): ApiJson<protocol::ArchiveAgentRunRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let project = require_project(&manager, &project_id).await?;
    let detail = project
        .get_agent_run_detail(&thread_id, &run_id)
        .await
        .map_err(core_error)?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "run_not_found",
                "AgentRun does not exist",
            )
        })?;
    if request.archived
        && matches!(
            detail.run.status,
            protocol::AgentRunStatus::Queued
                | protocol::AgentRunStatus::Running
                | protocol::AgentRunStatus::WaitingApproval
        )
    {
        return Err(api_error(
            StatusCode::CONFLICT,
            "run_active",
            "An active AgentRun cannot be archived",
        ));
    }
    if !project
        .archive_agent_run(&thread_id, &run_id, request.archived)
        .await
        .map_err(core_error)?
    {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "run_not_found",
            "AgentRun does not exist",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 向运行中的直接子 AgentRun 提交结构化用户输入。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/runs/{run_id}/input",
    operation_id = "submit_agent_input",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("run_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::AgentRunInputRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn submit_agent_input(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id, run_id)): ApiPath<(String, String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<protocol::AgentRunInputRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    let project = require_project(&manager, &project_id).await?;
    let detail = project
        .get_agent_run_detail(&thread_id, &run_id)
        .await
        .map_err(core_error)?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "run_not_found",
                "AgentRun does not exist",
            )
        })?;
    let Some(parent_run_id) = detail.run.parent_run_id.as_deref() else {
        return Err(api_error(
            StatusCode::CONFLICT,
            "run_not_child_agent",
            "User input is only available for child AgentRuns",
        ));
    };
    let parent = project
        .get_agent_run_detail(&thread_id, parent_run_id)
        .await
        .map_err(core_error)?
        .ok_or_else(|| {
            api_error(
                StatusCode::CONFLICT,
                "parent_run_unavailable",
                "Parent AgentRun is unavailable",
            )
        })?;
    if parent.run.parent_run_id.is_some() {
        return Err(api_error(
            StatusCode::CONFLICT,
            "run_not_direct_child",
            "User input is only available for direct child AgentRuns",
        ));
    }
    if is_terminal_run(detail.run.status) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "run_terminal",
            "Completed child AgentRuns cannot receive messages",
        ));
    }
    let command = submit_run_command_from_protocol_request_for_thread(
        protocol::SubmitRunRequest::InterveneMessage {
            input: request.input,
            client_echo_id: request.client_echo_id,
        },
        &thread,
    )
    .await
    .map_err(core_error)?;
    thread
        .send_task_input(run_id, detail.run.thread_id, command)
        .await
        .map_err(core_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 判断 AgentRun 是否已进入不再接受用户输入的终态。
fn is_terminal_run(status: protocol::AgentRunStatus) -> bool {
    matches!(
        status,
        protocol::AgentRunStatus::Completed
            | protocol::AgentRunStatus::Failed
            | protocol::AgentRunStatus::Cancelled
            | protocol::AgentRunStatus::Interrupted
    )
}

/// 向当前线程提交一次新的运行请求。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/runs",
    operation_id = "submit_run",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::SubmitRunRequest,
    responses((status = 202, description = "成功", body = omini_protocol::RunSubmittedResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn submit_run(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<protocol::SubmitRunRequest>,
) -> Result<(StatusCode, Json<protocol::RunSubmittedResponse>), crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    manager.ensure_bundled_rg().await.map_err(|error| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "bundled_tool_unavailable",
            error,
        )
    })?;
    let (is_initial_submit, input, is_init) = match &request {
        protocol::SubmitRunRequest::SubmitMessage { input, .. } => (true, input.clone(), false),
        protocol::SubmitRunRequest::InterveneMessage { input, .. } => (false, input.clone(), false),
        protocol::SubmitRunRequest::ExecuteCommand { command, input, .. } => (
            true,
            input.clone(),
            matches!(command, protocol::RunCommand::Init),
        ),
    };
    let command = submit_run_command_from_protocol_request_for_thread(request, &thread)
        .await
        .map_err(core_error)?;
    let command = thread.prepare_run(command).map_err(core_error)?;
    if is_initial_submit {
        // 同步落库 300 字符兜底 title。只有当 title 这次被实际写入
        // (text 非空 + DB 软写条件命中) 时,才 spawn 后台 LLM 升级任务,
        // 避免为后续每一次 submit 都创建无用的 tokio 任务。spawn 时把刚
        // 写入的兜底 title 一并传过去,LLM 跑完后会用它判断"title 仍然
        // 是我刚写入的兜底"才覆盖,避免覆盖用户的 /rename 或 fork 预设。
        let title_input = if is_init && fallback_thread_title_from_user_input(&input).is_none() {
            protocol::UserInput::plain("Initialize project")
        } else {
            input.clone()
        };
        let title_was_set = thread
            .set_initial_title_from_input(&title_input)
            .await
            .map_err(core_error)?;
        if title_was_set
            && !is_init
            && let Some(fallback_title) = fallback_thread_title_from_user_input(&input)
        {
            thread.spawn_background_title_generation(
                project_id,
                Arc::clone(&manager),
                fallback_title,
                input
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        protocol::InputPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            );
        }
    }
    thread
        .submit_prepared_run(command)
        .await
        .map(run_submitted_response_from_runtime_result)
        .map(|run| (StatusCode::ACCEPTED, Json(run)))
        .map_err(core_error)
}

/// 取消当前线程正在执行的运行。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/runs/{run_id}/cancel",
    operation_id = "cancel_run",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("run_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn cancel_run(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id, run_id)): ApiPath<(String, String, String)>,
    client_id: ClientId,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    if run_id == "current" {
        return thread
            .cancel_run()
            .await
            .map(|_| StatusCode::NO_CONTENT)
            .map_err(core_error);
    }
    let project = require_project(&manager, &project_id).await?;
    let run = project
        .get_agent_run_detail(&thread_id, &run_id)
        .await
        .map_err(core_error)?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "run_not_found",
                "AgentRun does not exist",
            )
        })?;
    if run.run.parent_run_id.is_some() {
        thread.cancel_agent_run(run_id).await.map_err(core_error)?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if !matches!(
        run.run.status,
        protocol::AgentRunStatus::Queued
            | protocol::AgentRunStatus::Running
            | protocol::AgentRunStatus::WaitingApproval
    ) {
        return Ok(StatusCode::NO_CONTENT);
    }
    thread
        .cancel_run()
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 处理当前线程中的工具权限请求
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/tool-pauses/{tool_use_id}/resolve",
    operation_id = "resolve_tool_pause",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("tool_use_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::ResolveToolPauseRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn resolve_tool_pause(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id, tool_use_id)): ApiPath<(String, String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<protocol::ResolveToolPauseRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let client_id = client_id.0;
    match thread
        .begin_tool_pause_resolution(client_id, &tool_use_id)
        .await
    {
        ToolPauseResolutionStart::Started => {
            // runtime 启动即加载,这里不再需要 ensure_loaded 等待。
        }
        ToolPauseResolutionStart::AlreadyResolved => {
            return Ok(StatusCode::NO_CONTENT);
        }
        ToolPauseResolutionStart::ClientNotConnected => {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "client_not_connected",
                "This client is not connected to the thread event stream",
            ));
        }
    }
    thread
        .resolve_tool_pause(resolve_tool_pause_command_from_protocol_request(
            tool_use_id,
            request,
        ))
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 审批或拒绝当前线程中的计划请求。
///
/// `ApproveInNewThread` 不走 core 审批状态机:server 路由层在调用 core 之前先
/// 读 plan 文件并 fork 新 `RuntimeThread`,通过 runtime event 通道广播
/// `ThreadSwitched`;core 收到此 action 后只负责关闭审批抽屉,不改状态。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/plans/plan/resolve",
    operation_id = "resolve_plan",
    tag = "runs",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::ResolvePlanRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn resolve_plan(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<protocol::ResolvePlanRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    if let protocol::PlanApprovalAction::ApproveInNewThread { profile } = request.action {
        // 「在新线程中执行计划」:在调用 core.resolve_plan 之前先 fork,避免
        // 旧 thread 的 plan 审批状态被 core 重复处理(后端实际只关闭抽屉)。
        // 如果 fork 失败,旧 thread 的 core 不应收到 resolve_plan,保持抽屉
        // 等待用户重试。
        let project = require_project(&manager, &project_id).await?;
        project
            .fork_thread_for_plan(&thread_id, profile)
            .await
            .map_err(core_error)?;
    }
    // core 收到 ApproveInNewThread 时只发出 resolved 事件关闭旧 thread
    // 的审批抽屉,不改 active_profile、不注入 plan 消息、不启动 run。
    thread
        .resolve_plan(resolve_plan_command_from_protocol_request(
            "plan".to_string(),
            request,
        ))
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}
