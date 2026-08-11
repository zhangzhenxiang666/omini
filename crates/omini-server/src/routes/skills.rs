use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use omini_protocol as protocol;
use std::sync::Arc;

use crate::daemon::GlobalDaemonManager;
use crate::event::bridge::{
    skill_response_from_runtime_skill_detail, skills_response_from_runtime_skill_summaries,
};
use crate::routes::{ApiResult, api_error, require_daemon_thread};

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

/// 获取指定技能的详细内容。
#[axum::debug_handler]
pub async fn get_skill(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, thread_id, skill_name)): Path<(String, String, String)>,
) -> ApiResult<protocol::SkillResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    thread
        .get_skill(&skill_name)
        .map(skill_response_from_runtime_skill_detail)
        .map(Json)
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "skill_not_found",
                format!("Skill '{skill_name}' does not exist"),
            )
        })
}
