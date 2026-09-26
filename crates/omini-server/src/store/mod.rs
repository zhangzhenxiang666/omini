use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::{DateTime, Utc};
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::task::TaskStatus;
use omini_domain::usage::Usage;
use omini_model::message::{ContentBlock, Message, Role};
use omini_runtime_contract::persistence::{RuntimePersistenceEvent, ThreadRecord};
use omini_runtime_contract::thread_domain::{
    AgentTaskExecutionMode, AgentTaskInfo, AgentTaskResult,
};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

mod agent_runs;
mod agent_tasks;
mod attachments;
mod content;
mod context;
mod database;
mod messages;
mod models;
mod persistence;
mod projects;
mod schema;
mod task_store;
mod threads;

pub use content::{CONTENT_SIZE_THRESHOLD, PreparedBlocks, prepare_blocks};
use content::{
    PreparedUiContent, cleanup_created_files, finish_prepared_write, prepare_ui_content,
};
pub(crate) use content::{
    load_asset, load_blocks, load_ui_content, persist_staged_asset, stored_asset_path,
};
pub use database::{Database, StoreError};
pub use models::{AgentTask, Attachment, NewMessage, Project, StoredMessage, Thread};
use models::{AgentTaskRow, StoredLlmMessageRow, StoredMessageRow, ThreadRow};
pub use threads::thread_from_runtime;
