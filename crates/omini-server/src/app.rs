use crate::routes;
use crate::runtime::GlobalDaemonManager;
use axum::Router;
use axum::extract::FromRef;
use axum::routing::{delete, get, post, put};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::oneshot;

#[derive(Clone)]
pub(crate) struct AppState {
    manager: Arc<GlobalDaemonManager>,
    shutdown: ShutdownTrigger,
}

#[derive(Clone)]
pub(crate) struct ShutdownTrigger {
    // shutdown endpoint 可能被重复调用，Sender 放在 Option 里保证只触发一次。
    tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl ShutdownTrigger {
    pub(crate) fn trigger(&self) -> bool {
        let Some(tx) = self.tx.lock().expect("shutdown lock poisoned").take() else {
            return false;
        };
        tx.send(()).is_ok()
    }
}

impl FromRef<AppState> for Arc<GlobalDaemonManager> {
    fn from_ref(state: &AppState) -> Self {
        state.manager.clone()
    }
}

impl FromRef<AppState> for ShutdownTrigger {
    fn from_ref(state: &AppState) -> Self {
        state.shutdown.clone()
    }
}

pub(crate) fn shutdown_channel() -> (ShutdownTrigger, oneshot::Receiver<()>) {
    let (tx, rx) = oneshot::channel();
    (
        ShutdownTrigger {
            tx: Arc::new(Mutex::new(Some(tx))),
        },
        rx,
    )
}

pub(crate) fn router(manager: Arc<GlobalDaemonManager>, shutdown: ShutdownTrigger) -> Router {
    let state = AppState { manager, shutdown };
    Router::new()
        // 守护进程级别接口，不绑定具体项目或会话。
        .route("/v1/health", get(routes::health::daemon_health))
        .route("/v1/shutdown", post(routes::shutdown::shutdown_daemon))
        .route("/v1/clients", post(routes::clients::register_client))
        // 项目 attach 是 daemon 认识项目的入口；其余项目接口都依赖这个注册关系。
        .route(
            "/v1/projects/{project_id}/attach",
            put(routes::projects::attach_project),
        )
        // 会话列表和会话创建作用在项目层级，具体会话操作继续挂在 session_id 下。
        .route(
            "/v1/projects/{project_id}/sessions",
            get(routes::sessions::list_sessions).post(routes::sessions::create_session),
        )
        .route(
            "/v1/projects/{project_id}/sessions/statuses",
            get(routes::sessions::list_session_statuses),
        )
        // WebSocket 订阅承载快照、历史 replay、运行时事件和控制权变化。
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/events",
            get(routes::sessions::session_events),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/status",
            get(routes::sessions::session_status),
        )
        // controller 接口只变更会话控制权，不直接驱动 core 运行。
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/controller/claim",
            post(routes::controllers::claim_controller),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/controller/release",
            post(routes::controllers::release_controller),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/controller/takeover",
            post(routes::controllers::takeover_controller),
        )
        // 会话配置类 mutation 在 handler 内区分自动接管和严格 controller 门禁。
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/models",
            get(routes::sessions::list_models),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/model",
            post(routes::sessions::set_model),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/thinking-effort",
            post(routes::sessions::set_thinking_effort),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/profile",
            post(routes::sessions::set_profile),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/profile/toggle",
            post(routes::sessions::toggle_profile),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/thinking-display",
            post(routes::sessions::set_thinking_display),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/open",
            post(routes::sessions::open_session),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/new",
            post(routes::sessions::new_session),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/rename",
            post(routes::sessions::rename_session),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/compact",
            post(routes::sessions::compact_context),
        )
        // 子代理和技能是当前项目/会话的可用能力清单。
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/agents",
            get(routes::agents::list_agents).post(routes::agents::save_agent),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/agents/generate",
            post(routes::agents::generate_agent),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/agents/{agent_id}",
            delete(routes::agents::delete_agent),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/skills",
            get(routes::skills::list_skills),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/skills/{skill_name}",
            get(routes::skills::get_skill),
        )
        // 运行、权限暂停和计划审批会改变 core 状态，必须走 controller 保护。
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/runs",
            post(routes::runs::submit_run),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/runs/{run_id}/cancel",
            post(routes::runs::cancel_run),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/tool-pauses/{tool_use_id}/resolve",
            post(routes::runs::resolve_tool_pause),
        )
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/plans/{plan_id}/resolve",
            post(routes::runs::resolve_plan),
        )
        // 附件只登记会话内引用，具体使用由后续 run input 决定。
        .route(
            "/v1/projects/{project_id}/sessions/{session_id}/attachments",
            post(routes::attachments::upload_attachment),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_trigger_is_idempotent() {
        let (trigger, mut rx) = shutdown_channel();

        assert!(trigger.trigger());
        assert!(!trigger.trigger());
        assert!(rx.try_recv().is_ok());
    }
}
