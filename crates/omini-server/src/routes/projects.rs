use crate::daemon::GlobalDaemonManager;
use crate::routes::{ApiResult, core_error, project_attach_error, require_project};
use axum::Json;
use axum::extract::{Path, State};
use omini_protocol as protocol;
use std::path::PathBuf;
use std::sync::Arc;

/// 将项目工作目录挂载到当前守护进程。
#[axum::debug_handler]
pub async fn attach_project(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::ProjectAttachRequest>,
) -> ApiResult<protocol::ProjectAttachResponse> {
    manager
        .attach_project(&project_id, PathBuf::from(request.cwd))
        .await
        .map(Json)
        .map_err(project_attach_error)
}

/// 列出项目默认可用模型；不需要已有 session。
#[axum::debug_handler]
pub async fn list_models(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<protocol::ModelsResponse> {
    let project = require_project(&manager, &project_id)?;
    project.list_models().map(Json).map_err(core_error)
}

/// 设置项目默认模型；后续新建 session 会继承该配置。
pub async fn set_model(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::SetModelRequest>,
) -> ApiResult<protocol::ProjectRuntimeConfigResponse> {
    let project = require_project(&manager, &project_id)?;
    project.set_model(request).map(Json).map_err(core_error)
}

/// 设置项目默认 thinking effort。
#[axum::debug_handler]
pub async fn set_thinking_effort(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::SetThinkingEffortRequest>,
) -> ApiResult<protocol::ProjectRuntimeConfigResponse> {
    let project = require_project(&manager, &project_id)?;
    project
        .set_thinking_effort(request)
        .map(Json)
        .map_err(core_error)
}

/// 设置项目默认 thinking 块显示偏好。
#[axum::debug_handler]
pub async fn set_thinking_display(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::SetThinkingDisplayRequest>,
) -> ApiResult<protocol::ProjectRuntimeConfigResponse> {
    let project = require_project(&manager, &project_id)?;
    project
        .set_thinking_display(request)
        .map(Json)
        .map_err(core_error)
}
