use crate::daemon::GlobalDaemonManager;
use crate::event::bridge::{
    models_response_from_runtime_snapshot, set_active_profile_command_from_protocol_request,
    set_model_command_from_protocol_request, set_thinking_effort_command_from_protocol_request,
};
use crate::routes::{
    ApiResult, api_error, client_id_from_headers, core_error, ensure_connected_controller,
    ensure_controller, require_daemon_thread, require_project, require_thread,
};
use crate::ws;
use axum::Json;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use omini_protocol as client_proto;
use serde::Deserialize;
use std::sync::Arc;

/// 列出指定项目下的线程。
#[axum::debug_handler]
pub async fn list_threads(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
) -> ApiResult<client_proto::ThreadsResponse> {
    let project = require_project(&manager, &project_id).await?;
    project.list_threads().await.map(Json).map_err(core_error)
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ThreadStatusQuery {
    #[serde(default)]
    status: Option<String>,
}

/// 列出指定项目下当前活跃线程的运行状态。
#[axum::debug_handler]
pub async fn list_thread_statuses(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Query(query): Query<ThreadStatusQuery>,
) -> ApiResult<client_proto::ThreadStatusesResponse> {
    let project = require_project(&manager, &project_id).await?;
    let filter = parse_status_filter(query.status.as_deref())?;
    Ok(Json(project.list_thread_statuses(filter.as_deref()).await))
}

/// 获取当前活跃线程的运行状态。
#[axum::debug_handler]
pub async fn thread_status(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
) -> ApiResult<client_proto::ThreadRuntimeStatusResponse> {
    let project = require_project(&manager, &project_id).await?;
    let status = if let Some(thread) = project.cached_thread(&thread_id) {
        thread.runtime_status()
    } else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "thread_status_not_found",
            "Thread is not currently active",
        ));
    };
    Ok(Json(client_proto::ThreadRuntimeStatusResponse { status }))
}

/// 在指定项目下创建一个新线程。
#[axum::debug_handler]
pub async fn create_thread(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<client_proto::CreateThreadRequest>,
) -> ApiResult<client_proto::CreateThreadResponse> {
    let project = require_project(&manager, &project_id).await?;
    project
        .create_thread(request)
        .await
        .map(Json)
        .map_err(core_error)
}

/// 列出当前线程可切换的模型。
#[axum::debug_handler]
pub async fn list_models(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
) -> ApiResult<client_proto::ModelsResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    Ok(Json(models_response_from_runtime_snapshot(
        thread.list_models(),
    )))
}

/// 设置当前线程使用的模型。
#[axum::debug_handler]
pub async fn set_model(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<client_proto::SetModelRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
    thread
        .set_model(set_model_command_from_protocol_request(request))
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 设置当前线程的思考强度。
#[axum::debug_handler]
pub async fn set_thinking_effort(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<client_proto::SetThinkingEffortRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
    thread
        .set_thinking_effort(set_thinking_effort_command_from_protocol_request(request))
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 设置当前线程的活跃供应商配置。
#[axum::debug_handler]
pub async fn set_profile(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<client_proto::SetActiveProfileRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
    thread
        .set_active_profile(set_active_profile_command_from_protocol_request(request))
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 在当前线程中切换活跃供应商配置。
#[axum::debug_handler]
pub async fn toggle_profile(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
    thread
        .toggle_active_profile()
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 设置当前线程是否显示思考内容。
#[axum::debug_handler]
pub async fn set_thinking_display(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<client_proto::SetThinkingDisplayRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
    thread
        .set_thinking_display(request)
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 加载并打开指定的已有线程。
#[axum::debug_handler]
pub async fn open_thread(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<client_proto::OpenThreadRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_controller(&thread, &headers).await?;
    // target thread 的 runtime 已经在 `require_daemon_thread` 阶段被
    // `manager.thread(...)` 同步加载好,这里不再需要额外的 ensure_loaded。
    let _ = require_daemon_thread(&manager, &project_id, &request.thread_id).await?;
    Ok(Json(client_proto::AckResponse::ok()))
}

/// 重命名当前线程。
#[axum::debug_handler]
pub async fn rename_thread(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<client_proto::RenameThreadRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_controller_claimed(&thread, &headers).await?;
    let title = normalize_thread_title(request.title)?;
    thread
        .rename_thread(title)
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 对当前线程上下文执行压缩。
#[axum::debug_handler]
pub async fn compact_context(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<client_proto::CompactContextRequest>,
) -> ApiResult<client_proto::AckResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_connected_controller(&thread, &headers).await?;
    thread
        .compact_context(request.instructions)
        .await
        .map(|_| Json(client_proto::AckResponse::ok()))
        .map_err(core_error)
}

/// 建立当前线程的事件 WebSocket 订阅。
#[axum::debug_handler]
pub async fn thread_events(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
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
    match require_thread(&project, &thread_id).await {
        Ok(thread) => ws
            .on_upgrade(move |socket| {
                ws::handle_socket(socket, project, thread, thread_id, client_id)
            })
            .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn ensure_controller_claimed(
    thread: &crate::thread::ThreadRuntime,
    headers: &HeaderMap,
) -> Result<(), crate::routes::ApiError> {
    let client_id = client_id_from_headers(headers)?;
    if thread.is_controller(client_id).await {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "not_controller",
            "This client is observing the thread and cannot mutate it",
        ))
    }
}

fn normalize_thread_title(title: String) -> Result<String, crate::routes::ApiError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_thread_title",
            "请提供新名称，用法: /rename <新名称>",
        ));
    }
    Ok(title.chars().take(300).collect())
}

fn parse_status_filter(
    status: Option<&str>,
) -> Result<Option<Vec<client_proto::ThreadRuntimeState>>, crate::routes::ApiError> {
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

fn parse_runtime_state(value: &str) -> Option<client_proto::ThreadRuntimeState> {
    match value {
        "idle" => Some(client_proto::ThreadRuntimeState::Idle),
        "working" => Some(client_proto::ThreadRuntimeState::Working),
        "thinking" => Some(client_proto::ThreadRuntimeState::Thinking),
        "waiting" => Some(client_proto::ThreadRuntimeState::Waiting),
        "compacting" => Some(client_proto::ThreadRuntimeState::Compacting),
        _ => None,
    }
}

fn invalid_status_filter(value: &str) -> crate::routes::ApiError {
    api_error(
        StatusCode::BAD_REQUEST,
        "invalid_status_filter",
        format!("Invalid thread status filter: {value}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_thread_title_trims_and_limits_length() {
        let title = normalize_thread_title(format!("  {}  ", "a".repeat(400)))
            .expect("title should normalize");

        assert_eq!(title.len(), 300);
        assert!(title.chars().all(|ch| ch == 'a'));
    }

    #[test]
    fn normalize_thread_title_rejects_blank_title() {
        let error = normalize_thread_title("   ".to_string()).expect_err("title should reject");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(error.1.0.code, "invalid_thread_title");
    }

    #[test]
    fn parse_status_filter_accepts_comma_separated_states() {
        let states = parse_status_filter(Some("idle, working")).expect("filter should parse");

        assert_eq!(
            states,
            Some(vec![
                client_proto::ThreadRuntimeState::Idle,
                client_proto::ThreadRuntimeState::Working,
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
