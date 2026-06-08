use axum::Json;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use omini_protocol as protocol;
use serde::Deserialize;
use std::sync::Arc;

use crate::routes::{
    ApiResult, api_error, client_id_from_headers, core_error, ensure_connected_controller,
    ensure_controller, require_daemon_session, require_project, require_session,
};
use crate::runtime::GlobalDaemonManager;
use crate::ws;

/// 列出指定项目下的会话。
pub(crate) async fn list_sessions(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<protocol::SessionsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.list_sessions().await.map(Json).map_err(core_error)
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SessionStatusQuery {
    #[serde(default)]
    status: Option<String>,
}

/// 列出指定项目下当前活跃会话的运行状态。
pub(crate) async fn list_session_statuses(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Query(query): Query<SessionStatusQuery>,
) -> ApiResult<protocol::SessionStatusesResponse> {
    let project = require_project(&manager, &project_id).await?;
    let filter = parse_status_filter(query.status.as_deref())?;
    Ok(Json(project.list_session_statuses(filter.as_deref()).await))
}

/// 获取当前活跃会话的运行状态。
pub(crate) async fn session_status(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
) -> ApiResult<protocol::SessionRuntimeStatusResponse> {
    let project = require_project(&manager, &project_id).await?;
    let Some(status) = project.session_status(&session_id).await else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "session_status_not_found",
            "Session is not currently active",
        ));
    };
    Ok(Json(status))
}

/// 在指定项目下创建一个新会话。
pub(crate) async fn create_session(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::CreateSessionRequest>,
) -> ApiResult<protocol::CreateSessionResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .create_session(request)
        .await
        .map(Json)
        .map_err(core_error)
}

/// 列出当前会话可切换的模型。
pub(crate) async fn list_models(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
) -> ApiResult<protocol::ModelsResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    Ok(Json(session.core.list_models()))
}

/// 设置当前会话使用的模型。
pub(crate) async fn set_model(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SetModelRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .core
        .set_model(request)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 设置当前会话的思考强度。
pub(crate) async fn set_thinking_effort(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SetThinkingEffortRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .core
        .set_thinking_effort(request)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 设置当前会话的活跃供应商配置。
pub(crate) async fn set_profile(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SetActiveProfileRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .core
        .set_active_profile(request)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 在当前会话中切换活跃供应商配置。
pub(crate) async fn toggle_profile(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .core
        .toggle_active_profile()
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 设置当前会话是否显示思考内容。
pub(crate) async fn set_thinking_display(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::SetThinkingDisplayRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .set_thinking_display(request)
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 加载并打开指定的已有会话。
pub(crate) async fn open_session(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::OpenSessionRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_controller(&session, &headers).await?;
    let target = require_daemon_session(&manager, &project_id, &request.session_id).await?;
    target.ensure_loaded().await.map_err(core_error)?;
    Ok(Json(protocol::AckResponse::ok()))
}

/// 重命名当前会话。
pub(crate) async fn rename_session(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::RenameSessionRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_controller_claimed(&session, &headers).await?;
    let title = normalize_session_title(request.title)?;
    session
        .rename_session(title)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

async fn ensure_controller_claimed(
    session: &crate::runtime::RuntimeSession,
    headers: &HeaderMap,
) -> Result<(), crate::routes::ApiError> {
    let client_id = client_id_from_headers(headers)?;
    if session.is_controller(client_id).await {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "not_controller",
            "This client is observing the session and cannot mutate it",
        ))
    }
}

fn normalize_session_title(title: String) -> Result<String, crate::routes::ApiError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_session_title",
            "请提供新名称，用法: /rename <新名称>",
        ));
    }
    Ok(title.chars().take(300).collect())
}

fn parse_status_filter(
    status: Option<&str>,
) -> Result<Option<Vec<protocol::SessionRuntimeState>>, crate::routes::ApiError> {
    let Some(status) = status.map(str::trim).filter(|status| !status.is_empty()) else {
        return Ok(None);
    };

    let mut states = Vec::new();
    for raw in status.split(',') {
        let value = raw.trim();
        if value.is_empty() {
            return Err(invalid_status_filter(raw));
        }
        states.push(parse_runtime_state(value).ok_or_else(|| invalid_status_filter(value))?);
    }
    Ok(Some(states))
}

fn parse_runtime_state(value: &str) -> Option<protocol::SessionRuntimeState> {
    match value {
        "idle" => Some(protocol::SessionRuntimeState::Idle),
        "working" => Some(protocol::SessionRuntimeState::Working),
        "thinking" => Some(protocol::SessionRuntimeState::Thinking),
        "waiting" => Some(protocol::SessionRuntimeState::Waiting),
        "compacting" => Some(protocol::SessionRuntimeState::Compacting),
        _ => None,
    }
}

fn invalid_status_filter(value: &str) -> crate::routes::ApiError {
    api_error(
        StatusCode::BAD_REQUEST,
        "invalid_status_filter",
        format!("Invalid session status filter: {value}"),
    )
}

/// 对当前会话上下文执行压缩。
pub(crate) async fn compact_context(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<protocol::CompactContextRequest>,
) -> ApiResult<protocol::AckResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    ensure_connected_controller(&session, &headers).await?;
    session
        .core
        .compact_context(request.instructions)
        .await
        .map(|_| Json(protocol::AckResponse::ok()))
        .map_err(core_error)
}

/// 建立当前会话的事件 WebSocket 订阅。
pub(crate) async fn session_events(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let client_id = match client_id_from_headers(&headers) {
        Ok(client_id) => client_id.to_string(),
        Err(error) => return error.into_response(),
    };

    let project = match require_project(&manager, &project_id).await {
        Ok(project) => project,
        Err(error) => return error.into_response(),
    };
    match require_session(&project, &session_id).await {
        Ok(session) => ws
            .on_upgrade(move |socket| {
                ws::handle_socket(socket, project, session, session_id, client_id)
            })
            .into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_session_title_trims_and_limits_length() {
        let title = normalize_session_title(format!("  {}  ", "a".repeat(400)))
            .expect("title should normalize");

        assert_eq!(title.len(), 300);
        assert!(title.chars().all(|ch| ch == 'a'));
    }

    #[test]
    fn normalize_session_title_rejects_blank_title() {
        let error = normalize_session_title("   ".to_string()).expect_err("title should reject");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(error.1.0.code, "invalid_session_title");
    }

    #[test]
    fn parse_status_filter_accepts_comma_separated_states() {
        let states = parse_status_filter(Some("idle, working")).expect("filter should parse");

        assert_eq!(
            states,
            Some(vec![
                protocol::SessionRuntimeState::Idle,
                protocol::SessionRuntimeState::Working,
            ])
        );
    }

    #[test]
    fn parse_status_filter_rejects_unknown_state() {
        let error = parse_status_filter(Some("idle,busy")).expect_err("filter should reject");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(error.1.0.code, "invalid_status_filter");
    }
}
