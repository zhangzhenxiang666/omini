//! server-core runtime 通信契约类型。
//!
//! 本 crate 只定义 `omini-server` 和 `omini-core` agent runtime facade 共享的窄接口。
//! QueryEngine 内部事件和结构继续留在 core。

pub mod events;
pub mod mcp;
pub mod project;
pub mod thread;
pub mod thread_domain;

pub use events::RuntimeToServerEvent;
pub use project::{AgentManagementUpdate, DeleteProjectAgentCommand, SaveProjectAgentCommand};
