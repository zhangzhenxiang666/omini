use crate::routes::extract::ApiPath;
use axum::Json;
use axum::extract::State;
use omini_protocol as protocol;

use crate::event::bridge::skills_response_from_runtime_skill_summaries;
use crate::routes::{ApiResult, require_daemon_thread};

/// 组装本模块的 HTTP 路由和 OpenAPI 描述。
pub(crate) fn routes() -> utoipa_axum::router::OpenApiRouter<crate::app::AppState> {
    utoipa_axum::router::OpenApiRouter::new().routes(utoipa_axum::routes!(list_skills))
}

/// 列出当前线程可用的技能。
#[utoipa::path(
    get,
    path = "/v1/projects/{project_id}/threads/{thread_id}/skills",
    operation_id = "list_skills",
    tag = "skills",
    params(("project_id" = String, Path), ("thread_id" = String, Path)),
    responses((status = 200, description = "成功", body = omini_protocol::SkillsResponse), (status = "default", description = "API 错误", body = omini_protocol::ProtocolError))
)]
#[axum::debug_handler]
pub async fn list_skills(
    State(crate::app::AppState { manager, .. }): State<crate::app::AppState>,
    ApiPath((project_id, thread_id)): ApiPath<(String, String)>,
) -> ApiResult<protocol::SkillsResponse> {
    let thread = require_daemon_thread(&manager, &project_id, &thread_id).await?;
    Ok(Json(skills_response_from_runtime_skill_summaries(
        thread.list_skills(),
    )))
}
