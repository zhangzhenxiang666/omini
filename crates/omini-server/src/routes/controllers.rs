use crate::routes::extract::ApiPath;
use crate::routes::extract::ClientId;
use crate::routes::{ApiResult, api_error, require_daemon_thread};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use omini_protocol as protocol;

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new()
        .routes(utoipa_axum::routes!(claim_controller))
        .routes(utoipa_axum::routes!(release_controller))
        .routes(utoipa_axum::routes!(takeover_controller))
}

/// 为客户端声明当前线程的控制权。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/controller/claim",
    operation_id = "claim_controller",
    tag = "controllers",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    responses((status = 200, description = "成功", body = omini_protocol::ControllerLease), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn claim_controller(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    ClientId(client_id): ClientId,
) -> ApiResult<protocol::ControllerLease> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let Some(controller_id) = thread.claim_controller(client_id.clone()).await else {
        return Err(client_not_connected());
    };
    Ok(Json(protocol::ControllerLease {
        client_id,
        controller_id: Some(controller_id),
    }))
}

/// 释放客户端持有的当前线程控制权。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/controller/release",
    operation_id = "release_controller",
    tag = "controllers",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn release_controller(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    ClientId(client_id): ClientId,
) -> Result<StatusCode, crate::routes::ApiError> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    thread.release_controller(&client_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// 强制接管当前线程的控制权。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/threads/{thread_id}/controller/takeover",
    operation_id = "takeover_controller",
    tag = "controllers",
    params(("project_id" = String, Path), ("thread_id" = String, Path), ("x-omini-client-id" = String, Header, description = "已注册并连接的客户端 ID")),
    responses((status = 200, description = "成功", body = omini_protocol::ControllerLease), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn takeover_controller(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
    ClientId(client_id): ClientId,
) -> ApiResult<protocol::ControllerLease> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let Some(controller_id) = thread.takeover_controller(client_id.clone()).await else {
        return Err(client_not_connected());
    };
    Ok(Json(protocol::ControllerLease {
        client_id,
        controller_id: Some(controller_id),
    }))
}

fn client_not_connected() -> crate::routes::ApiError {
    api_error(
        StatusCode::FORBIDDEN,
        "client_not_connected",
        "This client is not connected to the thread event stream",
    )
}
