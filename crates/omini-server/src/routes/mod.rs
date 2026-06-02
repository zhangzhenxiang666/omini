//! HTTP handler 的共享错误转换、鉴权和会话查找工具。

use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use omini_protocol::ProtocolError;
use std::sync::Arc;

use crate::runtime::{
    GlobalDaemonManager, ProjectAttachError, ProjectLookupError, RuntimeSession, SessionError,
    SessionManager,
};

pub(crate) mod agents;
pub(crate) mod attachments;
pub(crate) mod clients;
pub(crate) mod controllers;
pub(crate) mod health;
pub(crate) mod projects;
pub(crate) mod runs;
pub(crate) mod sessions;
pub(crate) mod shutdown;
pub(crate) mod skills;

const CLIENT_ID_HEADER: &str = "x-omini-client-id";

// 路由层统一把业务错误压成协议错误，避免各 handler 手写不同的 HTTP envelope。
pub(crate) type ApiError = (StatusCode, Json<ProtocolError>);
pub(crate) type ApiResult<T> = Result<Json<T>, ApiError>;

pub(crate) fn api_error(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> ApiError {
    (status, Json(ProtocolError::new(code, message)))
}

pub(crate) fn core_error(error: omini_core::CoreError) -> ApiError {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "core_error",
        error.message().to_string(),
    )
}

pub(crate) async fn require_project(
    manager: &GlobalDaemonManager,
    project_id: &str,
) -> Result<Arc<SessionManager>, ApiError> {
    manager
        .project(project_id)
        .await
        .map_err(project_lookup_error)
}

pub(crate) async fn require_session(
    manager: &SessionManager,
    session_id: &str,
) -> Result<Arc<RuntimeSession>, ApiError> {
    manager
        .session(session_id)
        .await
        .map_err(session_lookup_error)
}

pub(crate) async fn require_daemon_session(
    manager: &GlobalDaemonManager,
    project_id: &str,
    session_id: &str,
) -> Result<Arc<RuntimeSession>, ApiError> {
    // 大多数 session endpoint 都需要先确认项目已 attach，再确认 session 属于该项目。
    let project = require_project(manager, project_id).await?;
    require_session(&project, session_id).await
}

pub(crate) fn client_id_from_headers(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(CLIENT_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            api_error(
                StatusCode::UNAUTHORIZED,
                "missing_client_id",
                "Mutating requests must include x-omini-client-id",
            )
        })
}

pub(crate) async fn ensure_controller(
    session: &RuntimeSession,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let client_id = client_id_from_headers(headers)?;
    if session.is_controller(client_id).await {
        // 严格 mutation 只允许当前 controller 执行，并在进入 core 前确保 runtime 已加载。
        session.ensure_loaded().await.map_err(core_error)
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "not_controller",
            "This client is observing the session and cannot mutate it",
        ))
    }
}

pub(crate) async fn ensure_connected_controller(
    session: &RuntimeSession,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let client_id = client_id_from_headers(headers)?;
    // 运行相关用户动作必须来自已连接 WebSocket 的客户端；请求会先接管 controller，
    // 再进入 core，由 controller 语义负责冲突裁决。
    if !session.is_client_connected(client_id).await {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "client_not_connected",
            "This client is not connected to the session event stream",
        ));
    }
    if session
        .takeover_controller(client_id.to_string())
        .await
        .is_none()
    {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "client_not_connected",
            "This client is not connected to the session event stream",
        ));
    }
    // 已连接客户端可以接管，确保随后 core 发出的事件会被同一个客户端接收。
    session.ensure_loaded().await.map_err(core_error)
}

pub(crate) fn project_attach_error(error: ProjectAttachError) -> ApiError {
    match error {
        ProjectAttachError::BadRequest(message) => {
            api_error(StatusCode::BAD_REQUEST, "invalid_project_attach", message)
        }
        ProjectAttachError::Config(message) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "config_error", message)
        }
        ProjectAttachError::Core(error) => core_error(error),
    }
}

fn project_lookup_error(error: ProjectLookupError) -> ApiError {
    match error {
        ProjectLookupError::NotFound => api_error(
            StatusCode::NOT_FOUND,
            "project_not_attached",
            "Project has not been attached to this daemon",
        ),
    }
}

fn session_lookup_error(error: SessionError) -> ApiError {
    match error {
        SessionError::NotFound => api_error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session does not exist",
        ),
        SessionError::Core(error) => core_error(error),
    }
}
