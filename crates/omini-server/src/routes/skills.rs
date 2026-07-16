use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use omini_protocol as protocol;
use std::sync::Arc;

use crate::daemon::GlobalDaemonManager;
use crate::event::bridge::{
    skill_response_from_runtime_skill_detail, skills_response_from_runtime_skill_summaries,
};
use crate::routes::{ApiResult, api_error, require_daemon_session};

/// 列出当前会话可用的技能。
#[axum::debug_handler]
pub async fn list_skills(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id)): Path<(String, String)>,
) -> ApiResult<protocol::SkillsResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    Ok(Json(skills_response_from_runtime_skill_summaries(
        session.list_skills(),
    )))
}

/// 获取指定技能的详细内容。
#[axum::debug_handler]
pub async fn get_skill(
    State(manager): State<Arc<GlobalDaemonManager>>,
    Path((project_id, session_id, skill_name)): Path<(String, String, String)>,
) -> ApiResult<protocol::SkillResponse> {
    let session = require_daemon_session(&manager, &project_id, &session_id).await?;
    session
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
