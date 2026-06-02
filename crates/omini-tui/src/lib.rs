mod app;
mod client;
mod clipboard;
mod command;
mod input;
mod markdown;
mod protocol;
mod render;
mod selection;
mod state;
mod subagents;
mod types;
mod update;
mod widgets;

pub use client::ProjectConnection;

pub fn run_ui(connection: ProjectConnection) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(app::run_ui_async(connection))
}
