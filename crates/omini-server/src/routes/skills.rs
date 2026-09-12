use axum::Json;
use axum::extract::{Path, State};
use omini_protocol as protocol;
use std::sync::Arc;

use crate::daemon::GlobalDaemonManager;
use crate::event::bridge::skills_response_from_runtime_skill_summaries;
use crate::routes::{ApiResult, require_daemon_thread};

/// 列出当前线程可用的技能。
#[axum::debug_handler]
pub async fn list_skills(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id)): Path<(String, String)>,
) -> ApiResult<protocol::SkillsResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    Ok(Json(skills_response_from_runtime_skill_summaries(
        thread.list_skills(),
    )))
}
