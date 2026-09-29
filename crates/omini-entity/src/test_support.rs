//! 集成测试与下游 crate 测试共用的夹具:临时目录、固定时间与样例实体。

use crate::{Thread, ThreadType};
use jiff::Timestamp;
use omini_domain::task::TaskStatus;
use omini_runtime_contract::persistence::ThreadRecord;
use omini_runtime_contract::thread_domain::{AgentTaskExecutionMode, AgentTaskInfo};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub const TEST_PROJECT_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct TestRoot {
    pub path: PathBuf,
}

impl TestRoot {
    /// 包装已存在的临时目录(重开既有数据库的测试场景)。
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn new() -> Self {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("omini-entity-{}-{sequence}", std::process::id()));
        fs::create_dir(&path).expect("test root should be created");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn fixed_time() -> Timestamp {
    "2026-08-20T00:00:00Z"
        .parse()
        .expect("fixed test time should be valid")
}

/// 主线程样例:归属测试项目、无父线程。
pub fn test_thread(id: &str) -> Thread {
    let now = fixed_time();
    Thread {
        id: id.to_string(),
        project_id: TEST_PROJECT_ID.to_string(),
        parent_thread_id: None,
        spawn_tool_use_id: None,
        thread_type: ThreadType::Main,
        agent_label: None,
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        thinking_effort: None,
        title: None,
        current_context_tokens: 0,
        total_tokens: 0,
        total_cached_tokens: 0,
        llm_context_version: 1,
        created_at: now,
        updated_at: now,
        project: Default::default(),
        parent: Default::default(),
        children: Default::default(),
        messages: Default::default(),
    }
}

/// 子 Agent 线程的运行时记录样例。
pub fn test_agent_thread(id: &str, parent_thread_id: &str) -> ThreadRecord {
    let now = fixed_time();
    ThreadRecord {
        id: id.to_string(),
        parent_thread_id: Some(parent_thread_id.to_string()),
        spawn_tool_use_id: Some(format!("tool_{id}")),
        thread_type: "agent".to_string(),
        agent_label: Some("general".to_string()),
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        thinking_effort: None,
        title: Some("Test agent".to_string()),
        current_context_tokens: 0,
        total_tokens: 0,
        total_cached_tokens: 0,
        llm_context_version: 1,
        created_at: now,
        updated_at: now,
    }
}

/// 子 Agent 任务信息样例:后台模式、挂到指定主线程。
pub fn test_agent_task(task_id: &str, thread_id: &str, owner_thread_id: &str) -> AgentTaskInfo {
    let now = fixed_time();
    AgentTaskInfo {
        task_id: task_id.to_string(),
        thread_id: thread_id.to_string(),
        parent_run_id: None,
        parent_task_id: None,
        owner_thread_id: owner_thread_id.to_string(),
        parent_thread_id: owner_thread_id.to_string(),
        spawn_tool_use_id: format!("tool_{task_id}"),
        agent: "general".to_string(),
        title: "Test agent".to_string(),
        depth: 1,
        execution_mode: AgentTaskExecutionMode::Background,
        status: TaskStatus::Running,
        result: None,
        created_at: now,
        updated_at: now,
        completed_at: None,
        notification_delivered: false,
    }
}
