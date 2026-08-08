use crate::daemon::GlobalDaemonManager;
use crate::event::bridge::{
    fallback_session_title_from_user_input, resolve_plan_command_from_protocol_request,
    resolve_tool_pause_command_from_protocol_request, run_submitted_response_from_runtime_result,
    submit_run_command_from_protocol_request_for_thread,
};
use crate::event::tool_pause::ToolPauseResolutionStart;
use crate::routes::{
    ApiResult, api_error, client_id_from_headers, core_error, ensure_connected_controller,
    require_daemon_session, require_project,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use omini_protocol as protocol;
use std::sync::Arc;

/// 向当前会话提交一次新的运行请求。
#[axum::debug_handler]
pub async fn submit_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SubmitRunRequest>,
) -> ApiResult<protocol::RunSubmittedResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    if request.mode == protocol::RunInputMode::Submit {
        // 同步落库 300 字符兜底 title。只有当 title 这次被实际写入
        // (text 非空 + DB 软写条件命中) 时,才 spawn 后台 LLM 升级任务,
        // 避免为后续每一次 submit 都创建无用的 tokio 任务。spawn 时把刚
        // 写入的兜底 title 一并传过去,LLM 跑完后会用它判断"title 仍然
        // 是我刚写入的兜底"才覆盖,避免覆盖用户的 /rename 或 fork 预设。
        let title_was_set = session
            .set_initial_title_from_input(&request.input)
            .await
            .map_err(core_error)?;
        if title_was_set
            && let Some(fallback_title) = fallback_session_title_from_user_input(&request.input)
        {
            session.spawn_background_title_generation(
                project_id,
                Arc::clone(&manager),
                fallback_title,
                request.input.text.clone(),
            );
        }
    }
    let command =
        submit_run_command_from_protocol_request_for_thread(request, &session.thread_dir())
            .map_err(core_error)?;
    session
        .submit_run(command)
        .await
        .map(run_submitted_response_from_runtime_result)
        .map(Json)
        .map_err(core_error)
}

/// 取消当前会话正在执行的运行。
#[axum::debug_handler]
pub async fn cancel_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, _run_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .cancel_run()
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 处理当前会话中的工具权限请求
#[axum::debug_handler]
pub async fn resolve_tool_pause(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, tool_use_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::ResolveToolPauseRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    let client_id = client_id_from_headers(&headers)?.to_string();
    match session
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
                "This client is not connected to the session event stream",
            ));
        }
    }
    session
        .resolve_tool_pause(resolve_tool_pause_command_from_protocol_request(
            tool_use_id,
            request,
        ))
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 审批或拒绝当前会话中的计划请求。
///
/// `ApproveInNewSession` 不走 core 审批状态机:server 路由层在调用 core 之前先
/// 读 plan 文件并 fork 新 `RuntimeSession`,通过 runtime event 通道广播
/// `SessionSwitched`;core 收到此 action 后只负责关闭审批抽屉,不改状态。
#[axum::debug_handler]
pub async fn resolve_plan(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, plan_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::ResolvePlanRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    if let protocol::PlanApprovalAction::ApproveInNewSession { profile } = request.action {
        // 「在新会话中执行计划」:在调用 core.resolve_plan 之前先 fork,避免
        // 旧 session 的 plan 审批状态被 core 重复处理(后端实际只关闭抽屉)。
        // 如果 fork 失败,旧 session 的 core 不应收到 resolve_plan,保持抽屉
        // 等待用户重试。
        let project = require_project(&manager, &project_id).await?;
        project
            .fork_thread_for_plan(&session_id, &plan_id, profile)
            .await
            .map_err(core_error)?;
    }
    // core 收到 ApproveInNewSession 时只发出 resolved 事件关闭旧 session
    // 的审批抽屉,不改 active_profile、不注入 plan 消息、不启动 run。
    session
        .resolve_plan(resolve_plan_command_from_protocol_request(plan_id, request))
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}
