use axum::Json;
use omini_protocol as protocol;
use uuid::Uuid;

/// 注册一个客户端并分配新的客户端 ID。
#[axum::debug_handler]
pub async fn register_client(
    Json(_request): Json<protocol::RegisterClientRequest>,
) -> Json<protocol::RegisterClientResponse> {
    Json(protocol::RegisterClientResponse {
        client_id: Uuid::new_v4().to_string(),
    })
}
