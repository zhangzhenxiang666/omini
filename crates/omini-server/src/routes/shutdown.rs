use crate::app::AppState;
use axum::extract::State;
use axum::http::StatusCode;

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new().routes(utoipa_axum::routes!(shutdown_daemon))
}

/// 请求 daemon 进入 graceful shutdown。
#[utoipa::path(
    post,
    path = "/v1/shutdown",
    operation_id = "shutdown_daemon",
    tag = "shutdown",
    responses((status = 204, description = "成功"), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn shutdown_daemon(State(state): State<AppState>) -> StatusCode {
    state.shutdown.trigger();
    StatusCode::NO_CONTENT
}
