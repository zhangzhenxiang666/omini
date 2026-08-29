use crate::daemon::GlobalDaemonManager;
use crate::routes::{ApiResult, core_error, project_error, require_project};
use axum::Json;
use axum::extract::{Path, State};
use omini_protocol as protocol;
use std::sync::Arc;

#[axum::debug_handler]
pub async fn list_projects(
    State(manager): State<Arc<GlobalDaemonManager>>,
) -> ApiResult<protocol::ProjectsResponse> {
    manager
        .list_projects()
        .await
        .map(Json)
        .map_err(project_error)
}

#[axum::debug_handler]
pub async fn create_project(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Json(request): Json<protocol::CreateProjectRequest>,
) -> ApiResult<protocol::ProjectSummary> {
    manager
        .register_project(request)
        .await
        .map(Json)
        .map_err(project_error)
}

#[axum::debug_handler]
pub async fn get_project(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<protocol::ProjectSummary> {
    manager
        .project_summary(&project_id)
        .await
        .map(Json)
        .map_err(project_error)
}

#[axum::debug_handler]
pub async fn update_project(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::UpdateProjectRequest>,
) -> ApiResult<protocol::ProjectSummary> {
    manager
        .update_project(&project_id, request)
        .await
        .map(Json)
        .map_err(project_error)
}

#[axum::debug_handler]
pub async fn open_project(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<protocol::OpenProjectResponse> {
    manager
        .open_project(&project_id)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 返回项目当前有效配置是否能创建 runtime。
#[axum::debug_handler]
pub async fn project_configuration(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<protocol::ProjectConfigurationResponse> {
    manager
        .project_configuration(&project_id)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 仅为缺少最小 provider/model 的项目写入首次配置。
#[axum::debug_handler]
pub async fn bootstrap_project_configuration(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::BootstrapProjectConfigurationRequest>,
) -> ApiResult<protocol::ProjectConfigurationResponse> {
    manager
        .bootstrap_project_configuration(&project_id, request)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 列出项目默认可用模型；不需要已有 thread。
#[axum::debug_handler]
pub async fn list_models(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<protocol::ModelsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.list_models().map(Json).map_err(core_error)
}

/// 设置项目默认模型；后续新建 thread 会继承该配置。
pub async fn set_model(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::SetModelRequest>,
) -> ApiResult<protocol::ProjectRuntimeConfigResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.set_model(request).map(Json).map_err(core_error)
}

/// 设置项目默认 thinking effort。
#[axum::debug_handler]
pub async fn set_thinking_effort(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::SetThinkingEffortRequest>,
) -> ApiResult<protocol::ProjectRuntimeConfigResponse> {
    let project = require_project(&manager, &project_id).await?;
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
    let project = require_project(&manager, &project_id).await?;
    project
        .set_thinking_display(request)
        .map(Json)
        .map_err(core_error)
}
