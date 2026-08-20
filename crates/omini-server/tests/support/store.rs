use chrono::{TimeZone, Utc};
use omini_config::project::ProjectDir;
use omini_domain::events::{AgentTaskExecutionMode, AgentTaskInfo, AgentTaskStatus};
use omini_runtime_contract::persistence::ThreadRecord;
use omini_server::store::{Database, Project, Thread};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub const TEST_PROJECT_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct TestRoot {
    pub path: PathBuf,
}

impl TestRoot {
    pub fn new() -> Self {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "omini-server-store-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test root should be created");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn fixed_time() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 0, 0, 0)
        .single()
        .expect("fixed test time should be valid")
}

pub async fn temp_db() -> (Database, ProjectDir, TestRoot) {
    let root = TestRoot::new();
    let db = Database::open(&root.path.join("omini.sqlite"))
        .await
        .unwrap();
    let now = fixed_time();
    db.create_project(&Project {
        id: TEST_PROJECT_ID.to_string(),
        name: "test project".to_string(),
        path: root.path.join("cwd").display().to_string(),
        storage_key: "test-project".to_string(),
        created_at: now,
        updated_at: now,
        last_opened_at: None,
    })
    .await
    .unwrap();
    (db, ProjectDir::from_path(root.path.join("project")), root)
}

pub fn test_thread(id: &str) -> Thread {
    let now = fixed_time();
    Thread {
        id: id.to_string(),
        project_id: TEST_PROJECT_ID.to_string(),
        parent_thread_id: None,
        spawn_tool_use_id: None,
        thread_type: "main".to_string(),
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
    }
}

pub fn test_agent_task(task_id: &str, thread_id: &str, owner_thread_id: &str) -> AgentTaskInfo {
    let now = fixed_time();
    AgentTaskInfo {
        task_id: task_id.to_string(),
        thread_id: thread_id.to_string(),
        parent_task_id: None,
        owner_thread_id: owner_thread_id.to_string(),
        parent_thread_id: owner_thread_id.to_string(),
        spawn_tool_use_id: format!("tool_{task_id}"),
        agent: "general".to_string(),
        title: "Test agent".to_string(),
        depth: 1,
        execution_mode: AgentTaskExecutionMode::Background,
        status: AgentTaskStatus::Running,
        result: None,
        created_at: now,
        updated_at: now,
        completed_at: None,
        notification_delivered: false,
    }
}

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
