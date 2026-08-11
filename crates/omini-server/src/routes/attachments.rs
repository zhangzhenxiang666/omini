use crate::daemon::GlobalDaemonManager;
use crate::routes::{ApiResult, ensure_controller, require_daemon_thread};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use omini_protocol as client_proto;
use std::sync::Arc;

/// 上传当前线程的附件元数据，并返回附件引用信息。
pub async fn upload_attachment(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<client_proto::AttachmentUploadResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    ensure_controller(&thread, &headers).await?;
    let mime_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let attachment_id = thread
        .persist_attachment(&body, &mime_type)
        .map_err(crate::routes::core_error)?;
    Ok(Json(client_proto::AttachmentUploadResponse {
        attachment: client_proto::AttachmentMetadata {
            attachment_id,
            mime_type,
            size: body.len() as u64,
            name: "attachment".to_string(),
        },
    }))
}
