//! HTTP 路由组装和 daemon 级 shutdown 信号。

use crate::daemon::GlobalDaemonManager;
use crate::routes;
use axum::Router;
use axum::http::StatusCode;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// Axum handler 共享的 daemon 状态。
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) manager: Arc<GlobalDaemonManager>,
    pub(crate) shutdown: ShutdownTrigger,
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

#[derive(utoipa::OpenApi)]
#[openapi(info(title = "Omini Local Daemon API", version = env!("CARGO_PKG_VERSION")))]
struct ApiDoc;

/// 组合各业务模块的路由与 OpenAPI 文档，并注入 daemon 状态。
pub fn router(manager: Arc<GlobalDaemonManager>, shutdown: ShutdownTrigger) -> Router {
    use utoipa::OpenApi;
    use utoipa_axum::router::OpenApiRouter;

    let state = AppState { manager, shutdown };
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(routes::health::routes())
        .merge(routes::shutdown::routes())
        .merge(routes::clients::routes())
        .merge(routes::projects::routes())
        .merge(routes::agents::routes())
        .merge(routes::threads::routes())
        .merge(routes::controllers::routes())
        .merge(routes::runs::routes())
        .merge(routes::attachments::routes())
        .merge(routes::skills::routes())
        .split_for_parts();
    let router = router.with_state(state);
    #[cfg(debug_assertions)]
    let router =
        router.merge(utoipa_swagger_ui::SwaggerUi::new("/docs").url("/v1/openapi.json", openapi));
    #[cfg(not(debug_assertions))]
    let router = router.route(
        "/v1/openapi.json",
        axum::routing::get(move || async move { axum::Json(openapi.clone()) }),
    );
    router
        .fallback(route_not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

/// 将未知路径转换为与其他接口一致的协议错误。
#[axum::debug_handler]
async fn route_not_found() -> routes::ApiError {
    routes::api_error(
        StatusCode::NOT_FOUND,
        "route_not_found",
        "Route does not exist",
    )
}

/// 将不支持的 HTTP 方法转换为与其他接口一致的协议错误。
#[axum::debug_handler]
async fn method_not_allowed() -> routes::ApiError {
    routes::api_error(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "Method is not allowed for this route",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_trigger_repeated_call_reports_single_delivery() {
        let (trigger, mut rx) = shutdown_channel();

        assert!(trigger.trigger());
        assert!(!trigger.trigger());
        assert!(rx.try_recv().is_ok());
    }
}
