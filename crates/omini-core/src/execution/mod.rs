//! core 面向宿主的执行契约。
//!
//! 本模块集中暴露执行实例、命令句柄、独占输出、权威快照与宿主接口。
//! 执行实现位于内部 `runtime`，Agent 定义与任务管理位于内部 `agent`。

pub(crate) mod handle;
pub(crate) mod host;
pub(crate) mod identity;
pub(crate) mod instance;
pub(crate) mod snapshot;

pub use handle::{AgentEvents, AgentHandle, AgentOutput, RunReservation};
pub use host::{AgentHost, AgentSession, AgentSessionModel, AgentSessionRequest, HostError};
pub use identity::RunId;
pub use instance::{AgentInstance, AgentInstanceConfig, AgentInstanceLoad};
pub use snapshot::{AgentLifecycle, AgentSnapshot, CurrentRun};
