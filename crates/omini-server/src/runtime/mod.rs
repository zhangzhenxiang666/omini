//! daemon 内部的项目、会话、事件重放和控制权状态。
//!
//! `omini-server` 在这里把 HTTP/WS 层的会话语义适配到 `omini-core`：创建或恢复
//! runtime session、维护客户端 presence、裁剪重连 replay、落盘 core persistence event，
//! 并把当前运行态投影成 protocol DTO。

use chrono::{DateTime, Utc};
use omini_core::AgentCoreSession;
use omini_core::CoreError;
use omini_core::config::project::ProjectDir;
use omini_core::config::settings::OminiRoot;
use omini_core::config::settings::UserConfig;
use omini_core::persistence::RuntimePersistenceEvent;
use omini_core::types::display::HistoryItem;
use omini_core::types::events::{ActiveProfile, LoadedSession, RuntimeToServerEvent};
use omini_core::types::message::{ContentBlock, Message, Role};
use omini_domain::project::sanitize_project_path as sanitize;
use omini_protocol as protocol;
use omini_protocol::RuntimeEvent;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::history;
use crate::store::{Database, Session};

mod adapter;
mod load_gate;
mod manager;
mod presence;
mod replay;
mod session;
mod status;
mod tool_pause;

use adapter::*;
pub(crate) use adapter::{
    agents_snapshot_to_protocol, models_snapshot_to_protocol, resolve_plan_command_from_protocol,
    resolve_tool_pause_command_from_protocol, run_submitted_to_protocol,
    set_active_profile_command_from_protocol, set_model_command_from_protocol,
    set_thinking_effort_command_from_protocol, skill_detail_to_protocol,
    skill_summaries_to_protocol, submit_run_command_from_protocol,
};
use load_gate::*;
use presence::*;
use replay::*;
use status::*;
use tool_pause::*;

pub(crate) use manager::{
    GlobalDaemonManager, ProjectAttachError, ProjectLookupError, SessionError, SessionManager,
};
pub(crate) use session::RuntimeSession;
pub(crate) use tool_pause::ToolPauseResolutionStart;
