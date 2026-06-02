use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use omini_protocol as protocol;
use std::sync::Arc;

use crate::routes::{ApiResult, core_error, ensure_controller, require_daemon_session};
use crate::runtime::GlobalDaemonManager;

/// 列出当前会话可用的子代理配置。
pub(crate) async fn list_agents(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
) -> ApiResult<protocol::AgentsResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    Ok(Json(session.core.list_agents()))
}

/// 保存或更新当前会话的子代理配置。
pub(crate) async fn save_agent(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SaveAgentRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_controller(&session, &headers).await?;
    session
        .core
        .save_agent(request)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 删除当前会话中的指定子代理配置。
pub(crate) async fn delete_agent(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, agent_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_controller(&session, &headers).await?;
    session
        .core
        .delete_agent(agent_id)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 根据请求内容生成新的子代理配置。
pub(crate) async fn generate_agent(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::GenerateAgentRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_controller(&session, &headers).await?;
    session
        .core
        .generate_agent(request)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}
