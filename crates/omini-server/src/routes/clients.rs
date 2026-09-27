use axum::{Json, http::StatusCode};
use omini_protocol as protocol;
use uuid::Uuid;

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new().routes(utoipa_axum::routes!(register_client))
}

/// 注册一个客户端并分配新的客户端 ID。
#[utoipa::path(
    post,
    path = "/v1/clients",
    operation_id = "register_client",
    tag = "clients",
    responses((status = 201, description = "成功", body = omini_protocol::RegisterClientResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn register_client() -> (StatusCode, Json<protocol::RegisterClientResponse>) {
    (
        StatusCode::CREATED,
        Json(protocol::RegisterClientResponse {
            client_id: Uuid::new_v4().to_string(),
        }),
    )
}
