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
///
/// 仅 debug 构建可用；release 构建不编译 `app::debug` 模块与 `tui-debug` 子命令。
#[cfg(debug_assertions)]
pub fn run_debug_ui() -> std::io::Result<()> {
    app::debug::run()
}
