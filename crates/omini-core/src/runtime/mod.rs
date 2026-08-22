pub mod active_run;
pub mod agent_management;
pub mod capabilities;
pub mod compact;
pub mod event_processor;
pub mod history;
pub mod manual_compact;
pub mod plan;
pub mod plan_approval;
pub mod run_loop;
pub mod service;
pub mod usage;

use crate::engine::{QueryContext, ToolPauseResolver};
use crate::skills::SkillRegistry;
use crate::subagents::AgentRegistry;
use crate::tools::{ToolRegistry, ToolRuntimeContext};
use crate::types::events::EngineToRuntimeEvent;
use chrono::Utc;
use omini_config::Settings;
use omini_domain::config::ThinkingEffort;
use omini_domain::display::{DisplaySummary, UserDraft};
use omini_domain::events::{
    ActiveProfile, Notification, PlanApprovalAction, SubmittedPlan, ThreadUsageSnapshot,
    ToolPauseKind, ToolPauseRequest, ToolPauseResponse,
};
use omini_domain::message::Message;
use omini_domain::usage::Usage;
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use omini_runtime_contract::{RuntimeToServerEvent, ServerToRuntimeEvent};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use uuid::Uuid;

pub use capabilities::CapabilityStore;
pub use service::AgentRuntime;
pub use service::AgentRuntimeChannels;
pub use service::AgentRuntimeDeps;
pub use service::RuntimeCapabilityHandles;

/// 把已批准 plan 包装为新线程首条 user message 的公开入口,server 端 fork 时调用。
///
/// 内部细节保留在私有 `plan` 模块里,只暴露此最小函数。
pub fn compacted_plan_context(plan_content: &str) -> String {
    plan::compacted_context(plan_content)
}
