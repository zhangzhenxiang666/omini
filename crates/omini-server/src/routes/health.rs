use axum::Json;
use axum::extract::State;
use omini_protocol as protocol;

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new().routes(utoipa_axum::routes!(daemon_health))
}

/// 返回守护进程健康状态，用于客户端探活。
#[utoipa::path(
    get,
    path = "/v1/health",
    operation_id = "daemon_health",
    tag = "health",
    responses((status = 200, description = "成功", body = omini_protocol::DaemonHealthResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn daemon_health(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
) -> Json<protocol::DaemonHealthResponse> {
    Json(protocol::DaemonHealthResponse {
        ok: true,
        daemon: "omini-server".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_revision: protocol::PROTOCOL_REVISION,
        bundled_rg: manager.bundled_tool_status(),
    })
}
