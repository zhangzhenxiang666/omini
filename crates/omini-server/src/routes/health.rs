use crate::daemon::GlobalDaemonManager;
use axum::Json;
use axum::extract::State;
use omini_protocol as protocol;
use std::sync::Arc;

/// 返回守护进程健康状态，用于客户端探活。
#[axum::debug_handler]
pub async fn daemon_health(
    State(manager): State<Arc<GlobalDaemonManager>>,
) -> Json<protocol::DaemonHealthResponse> {
    Json(protocol::DaemonHealthResponse {
        ok: true,
        daemon: "omini-server".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_revision: protocol::PROTOCOL_REVISION,
        bundled_rg: manager.bundled_tool_status(),
    })
}
