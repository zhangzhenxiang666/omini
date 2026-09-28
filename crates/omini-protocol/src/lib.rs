//! client 和本地 daemon 之间的 HTTP/WebSocket 协议类型。
//!
//! 这个 crate 只描述 wire shape；运行时状态、配置加载和 UI 展示逻辑分别留在
//! `omini-core`、`omini-server` 和 `omini-tui`。

use jiff::Timestamp;

use omini_domain::conversation::{
    AssistantMessage, SystemEvent, UserInput as ConversationUserInput,
};
pub use omini_domain::subagents::AgentRecord as RuntimeAgentRecord;
pub use omini_domain::task::{
    TaskChangedEvent, TaskInfo, TaskKind, TaskOutputDelta, TaskOutputStream, TaskStatus,
};
pub use omini_model::message::{ToolResultBlock, ToolUseBlock};
pub use omini_runtime_contract::thread_domain::{
    AgentTaskEvent, AgentTaskEventEnvelope, AgentTaskExecutionMode, AgentTaskInfo, AgentTaskResult,
    AgentTaskSnapshot, CompactTrigger, MAX_AGENT_DEPTH, SubmittedPlan, ThreadUsage,
    ThreadUsageSnapshot,
};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_REVISION: u32 = 9;

/// 用户时间线快照中的一个条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "entry", rename_all = "snake_case")]
pub enum HistoryItem {
    UserInput(ConversationUserInput),
    AssistantMessage(AssistantMessage),
    SystemEvent(SystemEvent),
}

impl From<omini_domain::conversation::ConversationEntry> for HistoryItem {
    fn from(entry: omini_domain::conversation::ConversationEntry) -> Self {
        match entry {
            omini_domain::conversation::ConversationEntry::UserInput(input) => {
                Self::UserInput(input)
            }
            omini_domain::conversation::ConversationEntry::AssistantMessage(output) => {
                Self::AssistantMessage(output)
            }
            omini_domain::conversation::ConversationEntry::SystemEvent(output) => {
                Self::SystemEvent(output)
            }
        }
    }
}

mod models;
pub use models::*;
mod input;
pub use input::*;
mod pause;
pub use pause::*;
mod daemon;
pub use daemon::*;
mod projects;
pub use projects::*;
mod events;
pub use events::*;
mod threads;
pub use threads::*;
mod agents;
pub use agents::*;
mod skills;
pub use skills::*;
mod runs;
pub use runs::*;

mod controllers;
pub use controllers::*;
mod attachments;
pub use attachments::*;
mod error;
pub use error::*;
