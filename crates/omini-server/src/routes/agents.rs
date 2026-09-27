use crate::routes::extract::{ApiJson, ApiPath, ApiQuery};
use crate::routes::{ApiResult, core_error, require_project};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use omini_protocol as client_proto;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct AgentMutationQuery {
    #[serde(default)]
    target_thread_id: Option<String>,
}

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new()
        .routes(utoipa_axum::routes!(list_project_agents))
        .routes(utoipa_axum::routes!(save_project_agent))
        .routes(utoipa_axum::routes!(delete_project_agent))
        .routes(utoipa_axum::routes!(generate_project_agent))
}

/// 列出当前项目可用的子代理配置。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/agents",
    operation_id = "list_project_agents",
    tag = "agents",
    params(("project_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::AgentsResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_project_agents(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
) -> ApiResult<client_proto::AgentsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.list_agents().map(Json).map_err(core_error)
}

/// 保存或更新当前项目的子代理配置。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/agents",
    operation_id = "save_project_agent",
    tag = "agents",
    params(("project_id" = String, Path), ("target_thread_id" = Option<String>, Query, description = "更新后广播到指定线程")),
    request_body = omini_protocol::SaveAgentRequest,
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn save_project_agent(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiQuery(query): ApiQuery<AgentMutationQuery>,
    ApiJson(request): ApiJson<client_proto::SaveAgentRequest>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let project = require_project(&manager, &project_id).await?;
    project
        .save_agent(request, query.target_thread_id.as_deref())
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 删除当前项目中的指定子代理配置。
#[utoipa::path(
    delete,
    path = "/v1/projects/{project_id}/agents/{agent_id}",
    operation_id = "delete_project_agent",
    tag = "agents",
    params(("project_id" = String, Path), ("agent_id" = String, Path), ("target_thread_id" = Option<String>, Query, description = "更新后广播到指定线程")),
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn delete_project_agent(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, agent_id)): ApiPath<(String, String)>,
    ApiQuery(query): ApiQuery<AgentMutationQuery>,
) -> Result<StatusCode, crate::routes::ApiError> {
    let project = require_project(&manager, &project_id).await?;
    project
        .delete_agent(&agent_id, query.target_thread_id.as_deref())
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(core_error)
}

/// 根据请求内容同步生成新的子代理草稿。
#[utoipa::path(
    post,
    path = "/v1/projects/{project_id}/agents/generate",
    operation_id = "generate_project_agent",
    tag = "agents",
    params(("project_id" = String, Path)),
    request_body = omini_protocol::GenerateAgentRequest,
    responses((status = 200, description = "成功", body = omini_protocol::GenerateAgentResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn generate_project_agent(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath(project_id): ApiPath<String>,
    ApiJson(request): ApiJson<client_proto::GenerateAgentRequest>,
) -> ApiResult<client_proto::GenerateAgentResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .generate_agent(request)
        .await
        .map(Json)
        .map_err(core_error)
}
