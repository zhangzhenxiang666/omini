pub(crate) mod active_run;
pub(crate) mod capabilities;
pub(crate) mod command;
pub(crate) mod compact;
pub(crate) mod event_sink;
pub(crate) mod history;
pub(crate) mod manual_compact;
pub(crate) mod plan;
pub(crate) mod plan_approval;
pub(crate) mod run_loop;
pub(crate) mod service;
pub(crate) mod usage;
pub(crate) mod user_input;

use crate::CoreError;
use crate::agent::AgentRegistry;
use crate::engine::{QueryContext, ToolPauseResolver};
use crate::error::RuntimeError;
use crate::skills::SkillRegistry;
use crate::tools::{ToolRegistry, ToolRuntimeContext};
use crate::types::events::EngineToRuntimeEvent;
use jiff::Timestamp;
use omini_config::Settings;
use omini_domain::config::ThinkingEffort;
use omini_domain::conversation::CompactionSummary;
use omini_domain::usage::Usage;
use omini_model::message::{Message, ToolResultBlock, ToolUseBlock};
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::thread_domain::{
    ActiveProfile, Notification, PlanApprovalAction, SubmittedPlan, ThreadUsageSnapshot,
};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use uuid::Uuid;

pub(crate) use capabilities::CapabilityStore;
pub(crate) use service::AgentRuntime;

/// 把已批准 plan 包装为新线程首条 user message 的公开入口,server 端 fork 时调用。
///
/// 内部细节保留在私有 `plan` 模块里,只暴露此最小函数。
pub fn compacted_plan_context(plan_content: &str) -> String {
    plan::compacted_context(plan_content)
}
