//! HTTP 路由组装和 daemon 级 shutdown 信号。

use crate::daemon::GlobalDaemonManager;
use crate::routes;
use axum::Router;
use axum::extract::FromRef;
use axum::routing::{delete, get, post};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// Axum handler 共享的 daemon 状态。
#[derive(Clone)]
pub(crate) struct AppState {
    manager: Arc<GlobalDaemonManager>,
    shutdown: ShutdownTrigger,
}

/// 可复制的关闭触发器；真正的 oneshot sender 只会被消费一次。
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

/// 创建供 HTTP handler 触发、serve loop 等待的关闭通道。
pub fn shutdown_channel() -> (ShutdownTrigger, oneshot::Receiver<()>) {
    let (tx, rx) = oneshot::channel();
    (
        ShutdownTrigger {
            tx: Arc::new(Mutex::new(Some(tx))),
        },
        rx,
    )
}

/// 构建 daemon 的完整 HTTP 路由树。
pub fn router(manager: Arc<GlobalDaemonManager>, shutdown: ShutdownTrigger) -> Router {
    let state = AppState { manager, shutdown };
    Router::new().nest("/v1", v1_routes()).with_state(state)
}

fn v1_routes() -> Router<AppState> {
    Router::new()
        // 守护进程级接口不绑定具体项目或线程。
        .route("/health", get(routes::health::daemon_health))
        .route("/shutdown", post(routes::shutdown::shutdown_daemon))
        .route("/clients", post(routes::clients::register_client))
        .route(
            "/projects",
            get(routes::projects::list_projects).post(routes::projects::create_project),
        )
        .route(
            "/projects/{project_id}",
            get(routes::projects::get_project).patch(routes::projects::update_project),
        )
        .route(
            "/projects/{project_id}/open",
            post(routes::projects::open_project),
        )
        .nest("/projects/{project_id}", project_routes())
}

fn project_routes() -> Router<AppState> {
    Router::new()
        .route("/models", get(routes::projects::list_models))
        .route("/model", post(routes::projects::set_model))
        .route(
            "/thinking-effort",
            post(routes::projects::set_thinking_effort),
        )
        .route(
            "/thinking-display",
            post(routes::projects::set_thinking_display),
        )
        .route(
            "/agents",
            get(routes::agents::list_project_agents).post(routes::agents::save_project_agent),
        )
        .route(
            "/agents/generate",
            post(routes::agents::generate_project_agent),
        )
        .route(
            "/agents/{agent_id}",
            delete(routes::agents::delete_project_agent),
        )
        // 线程列表和创建作用在项目层级，具体线程操作继续挂在 thread_id 下。
        .route(
            "/threads",
            get(routes::threads::list_threads).post(routes::threads::create_thread),
        )
        .route(
            "/threads/statuses",
            get(routes::threads::list_thread_statuses),
        )
        .nest("/threads/{thread_id}", thread_routes())
}

fn thread_routes() -> Router<AppState> {
    Router::new()
        // WebSocket 订阅承载快照、历史 replay、运行时事件和控制权变化。
        .route("/events", get(routes::threads::thread_events))
        .route("/status", get(routes::threads::thread_status))
        .merge(controller_routes())
        .merge(thread_configuration_routes())
        .merge(thread_lifecycle_routes())
        .merge(capability_routes())
        .merge(run_routes())
        // 附件只登记线程内引用，具体使用由后续 run input 决定。
        .route("/attachments", post(routes::attachments::upload_attachment))
}

fn controller_routes() -> Router<AppState> {
    // controller 接口只变更线程控制权，不直接驱动 core 运行。
    Router::new()
        .route(
            "/controller/claim",
            post(routes::controllers::claim_controller),
        )
        .route(
            "/controller/release",
            post(routes::controllers::release_controller),
        )
        .route(
            "/controller/takeover",
            post(routes::controllers::takeover_controller),
        )
}

fn thread_configuration_routes() -> Router<AppState> {
    // 配置类 mutation 在 handler 内区分自动接管和严格 controller 门禁。
    Router::new()
        .route("/models", get(routes::threads::list_models))
        .route("/model", post(routes::threads::set_model))
        .route(
            "/thinking-effort",
            post(routes::threads::set_thinking_effort),
        )
        .route("/profile", post(routes::threads::set_profile))
        .route("/profile/toggle", post(routes::threads::toggle_profile))
        .route(
            "/thinking-display",
            post(routes::threads::set_thinking_display),
        )
}

fn thread_lifecycle_routes() -> Router<AppState> {
    // 这些操作改变当前线程的打开、创建或展示状态，不直接提交 run。
    Router::new()
        .route("/open", post(routes::threads::open_thread))
        .route("/rename", post(routes::threads::rename_thread))
        .route("/compact", post(routes::threads::compact_context))
}

fn capability_routes() -> Router<AppState> {
    // 技能是当前线程的可用能力清单。
    Router::new()
        .route("/skills", get(routes::skills::list_skills))
        .route("/skills/{skill_name}", get(routes::skills::get_skill))
}

fn run_routes() -> Router<AppState> {
    // 运行、权限暂停和计划审批会改变 core 状态，必须走 controller 保护。
    Router::new()
        .route("/runs", post(routes::runs::submit_run))
        .route("/runs/{run_id}/cancel", post(routes::runs::cancel_run))
        .route(
            "/tool-pauses/{tool_use_id}/resolve",
            post(routes::runs::resolve_tool_pause),
        )
        .route("/plans/plan/resolve", post(routes::runs::resolve_plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_tree_builds() {
        let _ = v1_routes();
    }

    #[test]
    fn shutdown_trigger_is_idempotent() {
        let (trigger, mut rx) = shutdown_channel();

        assert!(trigger.trigger());
        assert!(!trigger.trigger());
        assert!(rx.try_recv().is_ok());
    }
}
