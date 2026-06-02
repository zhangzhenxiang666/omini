use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use omini_protocol as protocol;
use std::sync::Arc;

use crate::routes::{ApiResult, api_error, client_id_from_headers, require_daemon_session};
use crate::runtime::GlobalDaemonManager;

/// 为客户端声明当前会话的控制权。
pub(crate) async fn claim_controller(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::ControllerLease> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    let client_id = client_id_from_headers(&headers)?.to_string();
    let Some(controller_id) = session.claim_controller(client_id.clone()).await else {
        return Err(client_not_connected());
    };
    Ok(Json(protocol::ControllerLease {
        client_id,
        controller_id: Some(controller_id),
    }))
}

/// 释放客户端持有的当前会话控制权。
pub(crate) async fn release_controller(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    let client_id = client_id_from_headers(&headers)?;
    session.release_controller(client_id).await;
    Ok(Json(protocol::AckResponse::ok()))
}

/// 强制接管当前会话的控制权。
pub(crate) async fn takeover_controller(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::ControllerLease> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    let client_id = client_id_from_headers(&headers)?.to_string();
    let Some(controller_id) = session.takeover_controller(client_id.clone()).await else {
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
        "This client is not connected to the session event stream",
    )
}
