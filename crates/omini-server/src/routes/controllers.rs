use crate::daemon::GlobalDaemonManager;
use crate::routes::{ApiResult, api_error, client_id_from_headers, require_daemon_thread};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use omini_protocol as protocol;
use std::sync::Arc;

/// 为客户端声明当前线程的控制权。
#[axum::debug_handler]
pub async fn claim_controller(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::ControllerLease> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let client_id = client_id_from_headers(&headers)?.to_string();
    let Some(controller_id) = thread.claim_controller(client_id.clone()).await else {
        return Err(client_not_connected());
    };
    Ok(Json(protocol::ControllerLease {
        client_id,
        controller_id: Some(controller_id),
    }))
}

/// 释放客户端持有的当前线程控制权。
#[axum::debug_handler]
pub async fn release_controller(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let client_id = client_id_from_headers(&headers)?;
    thread.release_controller(client_id).await;
    Ok(Json(protocol::AckResponse::ok()))
}

/// 强制接管当前线程的控制权。
#[axum::debug_handler]
pub async fn takeover_controller(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::ControllerLease> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    let client_id = client_id_from_headers(&headers)?.to_string();
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
