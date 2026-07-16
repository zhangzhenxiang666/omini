//! server-core runtime 通信契约类型。
//!
//! 本 crate 只定义 `omini-server` 和 `omini-core` agent runtime facade 共享的窄接口。
//! QueryEngine 内部事件和结构继续留在 core。

pub mod events;
pub mod mcp;
pub mod persistence;
pub mod project;
pub mod session;

pub use events::{RuntimeToServerEvent, ServerToRuntimeEvent};
pub use persistence::{RuntimePersistenceEvent, SessionRecord};
pub use project::{AgentManagementUpdate, DeleteProjectAgentCommand, SaveProjectAgentCommand};
