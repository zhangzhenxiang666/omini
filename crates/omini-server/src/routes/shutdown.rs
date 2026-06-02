use crate::app::ShutdownTrigger;
use axum::Json;
use axum::extract::State;
use omini_protocol as protocol;

pub(crate) async fn shutdown_daemon(
    State(shutdown): State<ShutdownTrigger>,
) -> Json<protocol::AckResponse> {
    shutdown.trigger();
    Json(protocol::AckResponse::ok())
}
