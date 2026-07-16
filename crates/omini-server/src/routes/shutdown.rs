use crate::app::ShutdownTrigger;
use axum::Json;
use axum::extract::State;
use omini_protocol as protocol;

/// 请求 daemon 进入 graceful shutdown。
#[axum::debug_handler]
pub async fn shutdown_daemon(
    State(shutdown): State<ShutdownTrigger>,
) -> Json<protocol::AckResponse> {
    shutdown.trigger();
    Json(protocol::AckResponse::ok())
}
