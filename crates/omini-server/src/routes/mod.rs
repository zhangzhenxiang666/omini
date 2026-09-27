//! HTTP handler 的共享错误转换、鉴权和线程查找工具。

use crate::daemon::{GlobalDaemonManager, ProjectError};
use crate::project::{ProjectManager, ThreadError};
use crate::thread::ThreadRuntime;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use omini_protocol::ProtocolError;
use std::sync::Arc;

pub mod agents;
pub mod attachments;
pub mod clients;
pub mod controllers;
pub mod extract;
pub mod health;
pub mod projects;
pub mod runs;
pub mod shutdown;
pub mod skills;
pub mod threads;

/// 统一保存公开状态码和错误体，确保提取错误与业务错误使用相同协议。
#[derive(Debug)]
pub(crate) struct ApiError {
    pub status: StatusCode,
    pub error: ProtocolError,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.error)).into_response()
    }
}

pub(crate) type ApiResult<T> = Result<Json<T>, ApiError>;

pub(crate) fn api_error(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> ApiError {
    ApiError {
        status,
        error: ProtocolError::new(code, message),
    }
}

pub(crate) fn core_error(error: omini_core::CoreError) -> ApiError {
    let status = match &error {
        omini_core::CoreError::RuntimeClosed | omini_core::CoreError::RuntimeLoadInterrupted => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        omini_core::CoreError::ThreadNotFound => StatusCode::NOT_FOUND,
        omini_core::CoreError::InvalidInput {
            code: "attachment_not_found",
            ..
        } => StatusCode::NOT_FOUND,
        omini_core::CoreError::InvalidInput {
            code: "delivery_key_conflict" | "delivery_failed",
            ..
        } => StatusCode::CONFLICT,
        omini_core::CoreError::InvalidModelSelection { .. }
        | omini_core::CoreError::InvalidInput { .. } => StatusCode::BAD_REQUEST,
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

        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.error.code, "runtime_closed");
    }

    #[test]
    fn core_error_maps_invalid_model_to_bad_request() {
        let error = core_error(omini_core::CoreError::invalid_model_selection(
            "Unknown model 'test'",
        ));

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.error.code, "invalid_model_selection");
    }

    #[test]
    fn core_error_maps_missing_thread_to_not_found() {
        let error = core_error(omini_core::CoreError::ThreadNotFound);

        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.error.code, "thread_not_found");
    }

    #[test]
    fn project_conflict_maps_to_http_conflict() {
        let error = project_error(ProjectError::Conflict("busy".to_string()));

        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.error.code, "project_conflict");
    }

    #[test]
    fn missing_project_path_maps_to_http_conflict() {
        let error = project_error(ProjectError::MissingPath("/missing".to_string()));

        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.error.code, "project_path_missing");
    }
}
