use crate::event::bridge::{
    models_response_from_runtime_snapshot, set_active_profile_command_from_protocol_request,
    set_model_command_from_protocol_request, set_thinking_effort_command_from_protocol_request,
};
use crate::routes::extract::ClientId;
use crate::routes::extract::{ApiJson, ApiPath, ApiQuery, ApiWebSocketUpgrade};
use crate::routes::{
    ApiResult, api_error, core_error, require_daemon_thread, require_project, require_thread,
};
use crate::ws;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use omini_protocol as client_proto;
use serde::Deserialize;

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new()
        .routes(utoipa_axum::routes!(list_threads))
        .routes(utoipa_axum::routes!(list_thread_statuses))
        .routes(utoipa_axum::routes!(thread_status))
        .routes(utoipa_axum::routes!(create_thread))
        .routes(utoipa_axum::routes!(list_models))
        .routes(utoipa_axum::routes!(set_model))
        .routes(utoipa_axum::routes!(set_thinking_effort))
        .routes(utoipa_axum::routes!(set_profile))
        .routes(utoipa_axum::routes!(toggle_profile))
        .routes(utoipa_axum::routes!(rename_thread))
        .routes(utoipa_axum::routes!(compact_context))
        .routes(utoipa_axum::routes!(thread_events))
}

/// 列出指定项目下的线程。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads",
    operation_id = "list_threads",
    tag = "threads",
    params(("project_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::ThreadsResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_threads(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
) -> ApiResult<client_proto::ThreadsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.list_threads().await.map(Json).map_err(core_error)
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ThreadStatusQuery {
    #[serde(default)]
    status: Option<String>,
}

/// 列出指定项目下当前活跃线程的运行状态。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads/statuses",
    operation_id = "list_thread_statuses",
    tag = "threads",
    params(("project_id" = String, Path), ("status" = Option<String>, Query, description = "逗号分隔的线程状态")),
    responses((status = 200, description = "成功", body = omini_protocol::ThreadStatusesResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_thread_statuses(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiQuery(query): ApiQuery<ThreadStatusQuery>,
) -> ApiResult<client_proto::ThreadStatusesResponse> {
    let project = require_project(&manager, &project_id).await?;
    let filter = parse_status_filter(query.status.as_deref())?;
    Ok(Json(project.list_thread_statuses(filter.as_deref()).await))
}

/// 获取当前活跃线程的运行状态。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads/{thread_id}/status",
    operation_id = "thread_status",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::ThreadRuntimeStatus), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn thread_status(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
) -> ApiResult<client_proto::ThreadRuntimeStatus> {
    let project = require_project(&manager, &project_id).await?;
    let status = if let Some(thread) = project.cached_thread(&thread_id) {
        thread.runtime_status()
    } else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "thread_status_not_found",
            "Thread is not currently active",
        ));
    };
    Ok(Json(status))
}

/// 在指定项目下创建一个新线程。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads",
    operation_id = "create_thread",
    tag = "threads",
    params(("project_id" = String, Path)),
    request_body = omini_protocol::CreateThreadRequest,
    responses((status = 201, description = "成功", body = omini_protocol::CreateThreadResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn create_thread(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiJson(request): ApiJson<client_proto::CreateThreadRequest>,
) -> Result<(StatusCode, Json<client_proto::CreateThreadResponse>), crate::routes::ApiError> {
    let project = require_project(&manager, &project_id).await?;
    project
        .create_thread(request)
        .await
        .map(|thread| (StatusCode::CREATED, Json(thread)))
        .map_err(core_error)
}

/// 列出当前线程可切换的模型。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads/{thread_id}/models",
    operation_id = "threads_list_models",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::ModelsResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_models(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
) -> ApiResult<client_proto::ModelsResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    Ok(Json(models_response_from_runtime_snapshot(
        thread.list_models(),
    )))
}

/// 设置当前线程使用的模型。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/model",
    operation_id = "threads_set_model",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::SetModelRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn set_model(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<client_proto::SetModelRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    thread
        .set_model(set_model_command_from_protocol_request(request))
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 设置当前线程的思考强度。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/thinking-effort",
    operation_id = "threads_set_thinking_effort",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::SetThinkingEffortRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn set_thinking_effort(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<client_proto::SetThinkingEffortRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    thread
        .set_thinking_effort(set_thinking_effort_command_from_protocol_request(request))
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 设置当前线程的活跃供应商配置。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/profile",
    operation_id = "set_profile",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::SetActiveProfileRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn set_profile(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<client_proto::SetActiveProfileRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    thread
        .set_active_profile(set_active_profile_command_from_protocol_request(request))
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 在当前线程中切换活跃供应商配置。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/profile/toggle",
    operation_id = "toggle_profile",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn toggle_profile(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    thread
        .toggle_active_profile()
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 重命名当前线程。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/rename",
    operation_id = "rename_thread",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::RenameThreadRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn rename_thread(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<client_proto::RenameThreadRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.require_controller(&thread).await?;
    let title = normalize_thread_title(request.title)?;
    thread
        .rename_thread(title)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 对当前线程上下文执行压缩。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/compact",
    operation_id = "compact_context",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    request_body = omini_protocol::CompactContextRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn compact_context(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiJson(request): ApiJson<client_proto::CompactContextRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    client_id.take_control(&thread).await?;
    thread
        .compact_context(request.instructions)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 建立当前线程的事件 WebSocket 订阅。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads/{thread_id}/events",
    operation_id = "thread_events",
    tag = "threads",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    responses((status = 101, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn thread_events(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    client_id: ClientId,
    ApiWebSocketUpgrade(ws): ApiWebSocketUpgrade,
) -> impl IntoResponse {
    let client_id = client_id.0;

    let project = match require_project(&manager, &project_id).await {
        Ok(project) => project,
        Err(error) => return error.into_response(),
    };
    match require_thread(&project, &thread_id).await {
        Ok(thread) => ws
            .on_upgrade(move |socket| {
                ws::handle_socket(socket, project, thread, thread_id, client_id)
            })
            .into_response(),
        Err(error) => error.into_response(),
    }
}
fn normalize_thread_title(title: String) -> Result<String, crate::routes::ApiError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_thread_title",
            "请提供新名称，用法: /rename <新名称>",
        ));
    }
    Ok(title.chars().take(300).collect())
}

fn parse_status_filter(
    status: Option<&str>,
) -> Result<Option<Vec<client_proto::ThreadRuntimeState>>, crate::routes::ApiError> {
    let Some(status) = status.map(str::trim).filter(|status| !status.is_empty()) else {
        return Ok(None);
    };

    let mut states = Vec::new();
    for raw in status.split(',') {
        let value = raw.trim();
        if value.is_empty() {
            return Err(invalid_status_filter(raw));
        }
        states.push(parse_runtime_state(value).ok_or_else(|| invalid_status_filter(value))?);
    }
    Ok(Some(states))
}

fn parse_runtime_state(value: &str) -> Option<client_proto::ThreadRuntimeState> {
    match value {
        "idle" => Some(client_proto::ThreadRuntimeState::Idle),
        "working" => Some(client_proto::ThreadRuntimeState::Working),
        "thinking" => Some(client_proto::ThreadRuntimeState::Thinking),
        "waiting" => Some(client_proto::ThreadRuntimeState::Waiting),
        "compacting" => Some(client_proto::ThreadRuntimeState::Compacting),
        _ => None,
    }
}

fn invalid_status_filter(value: &str) -> crate::routes::ApiError {
    api_error(
        StatusCode::BAD_REQUEST,
        "invalid_status_filter",
        format!("Invalid thread status filter: {value}"),
    )
}
