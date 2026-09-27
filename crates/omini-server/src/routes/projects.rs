use crate::routes::extract::{ApiJson, ApiPath};
use crate::routes::{ApiResult, core_error, project_error, require_project};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use omini_protocol as protocol;

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new()
        .routes(utoipa_axum::routes!(list_projects))
        .routes(utoipa_axum::routes!(create_project))
        .routes(utoipa_axum::routes!(get_project))
        .routes(utoipa_axum::routes!(update_project))
        .routes(utoipa_axum::routes!(open_project))
        .routes(utoipa_axum::routes!(project_configuration))
        .routes(utoipa_axum::routes!(bootstrap_project_configuration))
        .routes(utoipa_axum::routes!(list_models))
        .routes(utoipa_axum::routes!(set_model))
        .routes(utoipa_axum::routes!(set_thinking_effort))
}

/// 列出 daemon 已注册的项目。
#[utoipa::path(
    get,
    path = "/v1/projects",
    operation_id = "list_projects",
    tag = "projects",
    responses((status = 200, description = "成功", body = omini_protocol::ProjectsResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_projects(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
) -> ApiResult<protocol::ProjectsResponse> {
    manager
        .list_projects()
        .await
        .map(Json)
        .map_err(project_error)
}

/// 注册工作目录；同一路径的重复注册返回已有项目。
#[utoipa::path(
    post,
    path = "/v1/projects",
    operation_id = "create_project",
    tag = "projects",
    request_body = omini_protocol::CreateProjectRequest,
    responses((status = 201, description = "成功", body = omini_protocol::ProjectSummary), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn create_project(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiJson(request): ApiJson<protocol::CreateProjectRequest>,
) -> Result<(StatusCode, Json<protocol::ProjectSummary>), crate::routes::ApiError> {
    manager
        .register_project(request)
        .await
        .map(|project| (StatusCode::CREATED, Json(project)))
        .map_err(project_error)
}

/// 读取项目概要及当前路径可用状态。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}",
    operation_id = "get_project",
    tag = "projects",
    params(("project_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::ProjectSummary), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn get_project(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
) -> ApiResult<protocol::ProjectSummary> {
    manager
        .project_summary(&project_id)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 更新项目名称或重新关联工作目录。
#[utoipa::path(
    patch,
    path = "/v1/projects/{project_id}",
    operation_id = "update_project",
    tag = "projects",
    params(("project_id" = String, Path)),
    request_body = omini_protocol::UpdateProjectRequest,
    responses((status = 200, description = "成功", body = omini_protocol::ProjectSummary), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn update_project(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiJson(request): ApiJson<protocol::UpdateProjectRequest>,
) -> ApiResult<protocol::ProjectSummary> {
    manager
        .update_project(&project_id, request)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 打开项目并返回客户端初始化所需的线程与运行配置快照。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/open",
    operation_id = "open_project",
    tag = "projects",
    params(("project_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::OpenProjectResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn open_project(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
) -> ApiResult<protocol::OpenProjectResponse> {
    manager
        .open_project(&project_id)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 返回项目当前有效配置是否能创建 runtime。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/configuration",
    operation_id = "project_configuration",
    tag = "projects",
    params(("project_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::ProjectConfigurationResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn project_configuration(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
) -> ApiResult<protocol::ProjectConfigurationResponse> {
    manager
        .project_configuration(&project_id)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 仅为缺少最小 provider/model 的项目写入首次配置。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/configuration",
    operation_id = "bootstrap_project_configuration",
    tag = "projects",
    params(("project_id" = String, Path)),
    request_body = omini_protocol::BootstrapProjectConfigurationRequest,
    responses((status = 200, description = "成功", body = omini_protocol::ProjectConfigurationResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn bootstrap_project_configuration(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiJson(request): ApiJson<protocol::BootstrapProjectConfigurationRequest>,
) -> ApiResult<protocol::ProjectConfigurationResponse> {
    manager
        .bootstrap_project_configuration(&project_id, request)
        .await
        .map(Json)
        .map_err(project_error)
}

/// 列出项目默认可用模型；不需要已有 thread。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/models",
    operation_id = "projects_list_models",
    tag = "projects",
    params(("project_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::ModelsResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_models(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
) -> ApiResult<protocol::ModelsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.list_models().map(Json).map_err(core_error)
}

/// 设置项目默认模型；后续新建 thread 会继承该配置。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/model",
    operation_id = "projects_set_model",
    tag = "projects",
    params(("project_id" = String, Path)),
    request_body = omini_protocol::SetModelRequest,
    responses((status = 200, description = "成功", body = omini_protocol::ProjectRuntimeConfigResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn set_model(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiJson(request): ApiJson<protocol::SetModelRequest>,
) -> ApiResult<protocol::ProjectRuntimeConfigResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.set_model(request).map(Json).map_err(core_error)
}

/// 设置项目默认 thinking effort。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/thinking-effort",
    operation_id = "projects_set_thinking_effort",
    tag = "projects",
    params(("project_id" = String, Path)),
    request_body = omini_protocol::SetThinkingEffortRequest,
    responses((status = 200, description = "成功", body = omini_protocol::ProjectRuntimeConfigResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn set_thinking_effort(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiJson(request): ApiJson<protocol::SetThinkingEffortRequest>,
) -> ApiResult<protocol::ProjectRuntimeConfigResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .set_thinking_effort(request)
        .map(Json)
        .map_err(core_error)
}
