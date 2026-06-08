mod active_run;
mod agent_management;
mod capabilities;
pub(crate) mod compact;
mod event_processor;
mod history;
mod manual_compact;
mod plan;
mod plan_approval;
mod run_loop;
mod service;
mod session_lifecycle;
mod usage;

use crate::engine::{QueryContext, ToolPauseResolver};
use crate::persistence::{RuntimePersistenceEvent, SessionRecord};
use crate::skills::SkillRegistry;
use crate::subagents::AgentRegistry;
use crate::tools::{ToolRegistry, ToolRuntimeContext};
use crate::types::config::Settings;
use crate::types::config::ThinkingEffort;
use crate::types::display::{DisplaySummary, HistoryItem, UserDraft};
use crate::types::events::{
    ActiveProfile, EngineToRuntimeEvent, LoadedSession, Notification, PlanApprovalAction,
    RuntimeToServerEvent, ServerToRuntimeEvent, SessionUsageSnapshot, SubmittedPlan, ToolPauseKind,
    ToolPauseRequest, ToolPauseResponse,
};
use crate::types::message::Message;
use crate::types::usage::Usage;
use chrono::Utc;
use omini_domain::project::sanitize_project_path as sanitize;
use omini_provider_api::LlmClient;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::Ordering;
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

pub(crate) use capabilities::CapabilityStore;
pub use service::AgentRuntime;
pub(crate) use service::RuntimeCapabilityHandles;
