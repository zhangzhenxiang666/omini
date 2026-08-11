//! HTTP handler 的共享错误转换、鉴权和线程查找工具。

use crate::daemon::{GlobalDaemonManager, ProjectError};
use crate::project::{ProjectManager, ThreadError};
use crate::thread::ThreadRuntime;
use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use omini_protocol::ProtocolError;
use std::sync::Arc;

pub mod agents;
pub mod attachments;
pub mod clients;
pub mod controllers;
pub mod health;
pub mod projects;
pub mod runs;
pub mod shutdown;
pub mod skills;
pub mod threads;

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
        omini_core::CoreError::ThreadNotFound => StatusCode::NOT_FOUND,
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

pub async fn require_project(
    manager: &GlobalDaemonManager,
    project_id: &str,
) -> Result<Arc<ProjectManager>, ApiError> {
    manager
        .get_or_load_project(project_id)
        .await
        .map_err(project_error)
}

pub async fn require_thread(
    manager: &ProjectManager,
    thread_id: &str,
) -> Result<Arc<ThreadRuntime>, ApiError> {
    manager
        .get_or_load_thread(thread_id)
        .await
        .map_err(thread_lookup_error)
}

pub async fn require_daemon_thread(
    manager: &GlobalDaemonManager,
    project_id: &str,
    thread_id: &str,
) -> Result<Arc<ThreadRuntime>, ApiError> {
    let project = require_project(manager, project_id).await?;
    require_thread(&project, thread_id).await
}

pub fn client_id_from_headers(headers: &HeaderMap) -> Result<&str, ApiError> {
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

pub async fn ensure_controller(
    thread: &ThreadRuntime,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let client_id = client_id_from_headers(headers)?;
    if thread.is_controller(client_id).await {
        // 严格 mutation 只允许当前 controller 执行;新架构下 runtime 启动即
        // 加载,ThreadRuntime 一旦从 manager 拿到就已经持有完整 messages /
        // usage,不再需要等待 hydrate。
        Ok(())
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "not_controller",
            "This client is observing the thread and cannot mutate it",
        ))
    }
}

pub async fn ensure_connected_controller(
    thread: &ThreadRuntime,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let client_id = client_id_from_headers(headers)?;
    // 运行相关用户动作必须来自已连接 WebSocket 的客户端；请求会先接管 controller，
    // 再进入 core，由 controller 语义负责冲突裁决。
    if !thread.is_client_connected(client_id).await {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "client_not_connected",
            "This client is not connected to the thread event stream",
        ));
    }
    if thread
        .takeover_controller(client_id.to_string())
        .await
        .is_none()
    {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "client_not_connected",
            "This client is not connected to the thread event stream",
        ));
    }
    // 已连接客户端可以接管;runtime 已就绪,直接通过。
    Ok(())
}

pub fn project_error(error: ProjectError) -> ApiError {
    match error {
        ProjectError::NotFound => api_error(
            StatusCode::NOT_FOUND,
            "project_not_found",
            "Project does not exist",
        ),
        ProjectError::Invalid(message) => {
            api_error(StatusCode::BAD_REQUEST, "invalid_project", message)
        }
        ProjectError::Conflict(message) => {
            api_error(StatusCode::CONFLICT, "project_conflict", message)
        }
        ProjectError::MissingPath(path) => api_error(
            StatusCode::CONFLICT,
            "project_path_missing",
            format!("Project path '{path}' is not available"),
        ),
        ProjectError::Config(message) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "config_error", message)
        }
        ProjectError::Store(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "project_store_error",
            error.to_string(),
        ),
        ProjectError::Core(error) => core_error(error),
    }
}

fn thread_lookup_error(error: ThreadError) -> ApiError {
    match error {
        ThreadError::NotFound => api_error(
            StatusCode::NOT_FOUND,
            "thread_not_found",
            "Thread does not exist",
        ),
        ThreadError::Core(error) => core_error(error),
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
    fn core_error_maps_missing_thread_to_not_found() {
        let error = core_error(omini_core::CoreError::ThreadNotFound);

        assert_eq!(error.0, StatusCode::NOT_FOUND);
        assert_eq!(error.1.0.code, "thread_not_found");
    }

    #[test]
    fn project_conflict_maps_to_http_conflict() {
        let error = project_error(ProjectError::Conflict("busy".to_string()));

        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1.0.code, "project_conflict");
    }

    #[test]
    fn missing_project_path_maps_to_http_conflict() {
        let error = project_error(ProjectError::MissingPath("/missing".to_string()));

        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1.0.code, "project_path_missing");
    }
}
