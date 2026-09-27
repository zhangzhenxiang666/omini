pub mod app;
pub mod client;
pub mod features;
pub mod platform;
pub mod ui;

pub use client::{ConfigurationConnection, ProjectConnection, StartupConnection};

pub fn run_ui(connection: StartupConnection) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(app::run_ui_async(connection))
}

/// 打开无需服务端的 TUI 样式图鉴。
pub fn run_debug_ui() -> std::io::Result<()> {
    app::debug::run()
}
