use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use omini_protocol as protocol;
use std::sync::Arc;

use crate::routes::{
    ApiResult, api_error, client_id_from_headers, core_error, daemon_session_or_not_found,
    ensure_connected_controller,
};
use crate::runtime::{GlobalDaemonManager, ToolPauseResolutionStart};

/// 向当前会话提交一次新的运行请求。
pub(crate) async fn submit_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SubmitRunRequest>,
) -> ApiResult<protocol::RunSubmittedResponse> {
    let session = daemon_session_or_not_found(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    if request.mode == protocol::RunInputMode::Submit {
        session
            .set_initial_title_from_input(&request.input)
            .await
            .map_err(core_error)?;
    }
    session
        .core
        .submit_run(request)
        .await
        .map(Json)
        .map_err(core_error)
}

/// 取消当前会话正在执行的运行。
pub(crate) async fn cancel_run(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, _run_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::AckResponse> {
    let session = daemon_session_or_not_found(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .core
        .cancel_run()
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 响应当前会话中的工具权限暂停请求。
pub(crate) async fn resolve_tool_pause(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, tool_use_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::ResolveToolPauseRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = daemon_session_or_not_found(&manager, &project_id, &session_id).await?;
    let client_id = client_id_from_headers(&headers)?.to_string();
    match session
        .begin_tool_pause_resolution(client_id, &tool_use_id)
        .await
    {
        ToolPauseResolutionStart::Started => {
            session.ensure_loaded().await.map_err(core_error)?;
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
        .core
        .resolve_tool_pause(tool_use_id, request)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 审批或拒绝当前会话中的计划请求。
pub(crate) async fn resolve_plan(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, plan_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::ResolvePlanRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = daemon_session_or_not_found(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .core
        .resolve_plan(plan_id, request)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}
