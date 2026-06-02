use omini_core::config::settings::OminiRoot;
use omini_core::config::settings::UserConfig;
use std::io;
use std::sync::Arc;
use tokio::net::TcpListener;

mod app;
mod history;
pub mod process;
mod routes;
mod runtime;
pub mod runtime_state;
pub mod store;
mod ws;

/// 启动本地 daemon，并把实际监听端口写入运行状态文件供客户端发现。
pub async fn serve_daemon(root: OminiRoot, config: UserConfig) -> io::Result<()> {
    let db = store::Database::open(&root.db_path())
        .await
        .map_err(io::Error::other)?;
    let manager = Arc::new(runtime::GlobalDaemonManager::new(
        root,
        config,
        Arc::new(db),
    ));
    let (shutdown, shutdown_rx) = app::shutdown_channel();
    let app = app::router(manager, shutdown);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    // 端口由系统分配，所以必须在 bind 成功后再发布给 TUI/CLI。
    runtime_state::write(addr.port())?;

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
    // 无论 serve 正常退出还是返回错误，都尽量清理旧端口，避免客户端连到失效 daemon。
    runtime_state::cleanup();
    result
}
