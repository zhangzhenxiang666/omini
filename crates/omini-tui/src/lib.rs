pub use omini_runtime::config;
pub use omini_runtime::runtime;
pub use omini_types as types;
pub use omini_types::subagents;

mod app;
mod clipboard;
mod input;
mod markdown;
mod render;
mod selection;
mod state;
mod update;
mod widgets;

pub use app::run_ui;
