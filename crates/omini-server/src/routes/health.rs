use axum::Json;
use omini_protocol as protocol;

/// 返回守护进程健康状态，用于客户端探活。
#[axum::debug_handler]
pub async fn daemon_health() -> Json<protocol::DaemonHealthResponse> {
    Json(protocol::DaemonHealthResponse {
        ok: true,
        daemon: "omini-server".to_string(),
    })
}
