//! 本地 daemon 的 HTTP/WebSocket transport 层。
//!
//! 这个 crate 负责项目 attach、会话路由、控制权管理、事件 fanout 和 SQLite 持久化；
//! agent 执行逻辑仍由 `omini-core` 负责。

use omini_config::OminiRoot;
use std::io;
use std::sync::Arc;
use tokio::net::TcpListener;

mod app;
mod git;
mod history;
mod logging;
pub mod process;
mod routes;
mod runtime;
pub mod runtime_state;
pub mod store;
mod ws;

/// 启动本地 daemon，并把实际监听端口写入运行状态文件供客户端发现。
pub async fn serve_daemon(root: OminiRoot) -> io::Result<()> {
    tracing::info!("starting omini server daemon");
    let db = store::Database::open(&root.db_path())
        .await
        .map_err(io::Error::other)?;
    let manager = Arc::new(runtime::GlobalDaemonManager::new(root, Arc::new(db)));
    let (shutdown, shutdown_rx) = app::shutdown_channel();
    let app = app::router(manager, shutdown);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    // 端口由系统分配，所以必须在 bind 成功后再发布给 TUI/CLI。
    runtime_state::write(addr.port())?;
    tracing::info!(port = addr.port(), "omini server daemon listening");

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
    // 无论 serve 正常退出还是返回错误，都尽量清理旧端口，避免客户端连到失效 daemon。
    runtime_state::cleanup();
    tracing::info!("omini server daemon stopped");
    result
}
