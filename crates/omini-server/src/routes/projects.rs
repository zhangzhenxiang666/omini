use axum::Json;
use axum::extract::{Path, State};
use omini_protocol as protocol;
use std::path::PathBuf;
use std::sync::Arc;

use crate::routes::{ApiResult, project_attach_error};
use crate::runtime::GlobalDaemonManager;

/// 将项目工作目录挂载到当前守护进程。
pub(crate) async fn attach_project(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path(project_id): Path<String>,
    Json(request): Json<protocol::ProjectAttachRequest>,
) -> ApiResult<protocol::ProjectAttachResponse> {
    manager
        .attach_project(&project_id, PathBuf::from(request.cwd))
        .await
        .map(Json)
        .map_err(project_attach_error)
}
