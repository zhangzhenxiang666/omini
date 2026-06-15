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
    let status = match &error {
        omini_core::CoreError::RuntimeClosed | omini_core::CoreError::RuntimeLoadInterrupted => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        omini_core::CoreError::SessionNotFound => StatusCode::NOT_FOUND,
        omini_core::CoreError::InvalidModelSelection { .. } => StatusCode::BAD_REQUEST,
        omini_core::CoreError::Internal { .. }
        | omini_core::CoreError::Config { .. }
        | omini_core::CoreError::ProjectState { .. }
        | omini_core::CoreError::Persistence { .. }
        | omini_core::CoreError::RuntimeEventEncode { .. }
        | omini_core::CoreError::Subagent { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.code(), error.message().into_owned())
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
        // 严格 mutation 只允许当前 controller 执行;新架构下 runtime 启动即
        // 加载,RuntimeSession 一旦从 manager 拿到就已经持有完整 messages /
        // usage,不再需要等待 hydrate。
        Ok(())
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
    // 已连接客户端可以接管;runtime 已就绪,直接通过。
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_maps_runtime_closed_to_unavailable() {
        let error = core_error(omini_core::CoreError::RuntimeClosed);

        assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.1.0.code, "runtime_closed");
    }

    #[test]
    fn core_error_maps_invalid_model_to_bad_request() {
        let error = core_error(omini_core::CoreError::invalid_model_selection(
            "Unknown model 'test'",
        ));

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(error.1.0.code, "invalid_model_selection");
    }

    #[test]
    fn core_error_maps_missing_session_to_not_found() {
        let error = core_error(omini_core::CoreError::SessionNotFound);

        assert_eq!(error.0, StatusCode::NOT_FOUND);
        assert_eq!(error.1.0.code, "session_not_found");
    }
}
