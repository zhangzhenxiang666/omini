use crate::daemon::GlobalDaemonManager;
use crate::routes::{ApiResult, core_error, require_project};
use axum::Json;
use axum::extract::{Path, Query, State};
use omini_protocol as client_proto;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Default, Deserialize)]
pub struct AgentMutationQuery {
    #[serde(default)]
    target_thread_id: Option<String>,
}

/// 列出当前项目可用的子代理配置。
#[axum::debug_handler]
pub async fn list_project_agents(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<client_proto::AgentsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.list_agents().map(Json).map_err(core_error)
}

/// 保存或更新当前项目的子代理配置。
#[axum::debug_handler]
pub async fn save_project_agent(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Query(query): Query<AgentMutationQuery>,
    Json(request): Json<client_proto::SaveAgentRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .save_agent(request, query.target_thread_id.as_deref())
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 删除当前项目中的指定子代理配置。
#[axum::debug_handler]
pub async fn delete_project_agent(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, agent_id)): Path<(String, String)>,
    Query(query): Query<AgentMutationQuery>,
) -> ApiResult<client_proto::AckResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .delete_agent(&agent_id, query.target_thread_id.as_deref())
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 根据请求内容同步生成新的子代理草稿。
#[axum::debug_handler]
pub async fn generate_project_agent(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<client_proto::GenerateAgentRequest>,
) -> ApiResult<client_proto::GenerateAgentResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .generate_agent(request)
        .await
        .map(Json)
        .map_err(core_error)
}
