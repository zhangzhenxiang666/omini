use crate::daemon::GlobalDaemonManager;
use crate::event::bridge::{
    fallback_thread_title_from_user_input, resolve_plan_command_from_protocol_request,
    resolve_tool_pause_command_from_protocol_request, run_submitted_response_from_runtime_result,
    submit_run_command_from_protocol_request_for_thread,
};
use crate::event::tool_pause::ToolPauseResolutionStart;
use crate::routes::{
    ApiResult, api_error, client_id_from_headers, core_error, ensure_connected_controller,
    require_daemon_thread, require_project,
};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use omini_protocol as protocol;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Default, Deserialize)]
pub struct ListRunsQuery {
    #[serde(default)]
    include_archived: bool,
}

pub async fn list_agent_runs(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    Query(query): Query<ListRunsQuery>,
) -> ApiResult<protocol::AgentRunsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .list_agent_runs(&thread_id, query.include_archived)
        .await
        .map(Json)
        .map_err(core_error)
}

pub async fn get_agent_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id, run_id)): Path<(String, String, String)>,
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

pub async fn archive_agent_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id, run_id)): Path<(String, String, String)>,
    Json(request): Json<protocol::ArchiveAgentRunRequest>,
) -> ApiResult<protocol::AckResponse> {
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
    Ok(Json(protocol::AckResponse::ok()))
}

pub async fn intervene_agent_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id, run_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::AgentRunMessageRequest>,
) -> ApiResult<protocol::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
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
    let Some(parent_run_id) = detail.run.parent_run_id else {
        return Err(api_error(
            StatusCode::CONFLICT,
            "run_not_child_agent",
            "User intervention is only available for child AgentRuns",
        ));
    };
    let parent = project
        .get_agent_run_detail(&thread_id, &parent_run_id)
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
            "User intervention is only available for direct child AgentRuns",
        ));
    }
    let message = request.message.trim();
    if message.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "empty_message",
            "Message must not be empty",
        ));
    }
    thread
        .intervene_agent_run(
            run_id,
            omini_domain::message::Message::from_user_text(message.to_string()),
        )
        .await
        .map_err(core_error)?;
    Ok(Json(protocol::AckResponse::ok()))
}

/// 向当前线程提交一次新的运行请求。
#[axum::debug_handler]
pub async fn submit_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SubmitRunRequest>,
) -> ApiResult<protocol::RunSubmittedResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
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
        .map(Json)
        .map_err(core_error)
}

/// 取消当前线程正在执行的运行。
#[axum::debug_handler]
pub async fn cancel_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id, run_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
    if run_id == "current" {
        return thread
            .cancel_run()
            .await
            .map(|_| Json(protocol::AckResponse::ok()))
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
        return Ok(Json(protocol::AckResponse::ok()));
    }
    if !matches!(
        run.run.status,
        protocol::AgentRunStatus::Queued
            | protocol::AgentRunStatus::Running
            | protocol::AgentRunStatus::WaitingApproval
    ) {
        return Ok(Json(protocol::AckResponse::ok()));
    }
    thread
        .cancel_run()
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 处理当前线程中的工具权限请求
#[axum::debug_handler]
pub async fn resolve_tool_pause(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id, tool_use_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::ResolveToolPauseRequest>,
) -> ApiResult<protocol::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let client_id = client_id_from_headers(&headers)?.to_string();
    match thread
        .begin_tool_pause_resolution(client_id, &tool_use_id)
        .await
    {
        ToolPauseResolutionStart::Started => {
            // runtime 启动即加载,这里不再需要 ensure_loaded 等待。
        }
        ToolPauseResolutionStart::AlreadyResolved => {
            return Ok(Json(protocol::AckResponse::ok()));
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
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 审批或拒绝当前线程中的计划请求。
///
/// `ApproveInNewThread` 不走 core 审批状态机:server 路由层在调用 core 之前先
/// 读 plan 文件并 fork 新 `RuntimeThread`,通过 runtime event 通道广播
/// `ThreadSwitched`;core 收到此 action 后只负责关闭审批抽屉,不改状态。
#[axum::debug_handler]
pub async fn resolve_plan(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::ResolvePlanRequest>,
) -> ApiResult<protocol::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
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
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}
