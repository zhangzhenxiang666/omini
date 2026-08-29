mod app;
mod client;
mod clipboard;
mod command;
mod input;
mod markdown;
mod protocol;
mod render;
mod selection;
mod setup;
mod state;
mod terminal;
mod types;
mod update;
mod widgets;

pub use client::ConfigurationConnection;
pub use client::ProjectConnection;
pub use client::StartupConnection;

pub fn run_ui(connection: StartupConnection) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(app::run_ui_async(connection))
}
