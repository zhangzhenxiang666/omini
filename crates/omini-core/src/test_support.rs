use crate::execution::host::{AgentHost, AgentSession, AgentSessionRequest, HostError};
use crate::tools::{PendingToolPauses, ToolExecutionContext, ToolRegistry};
use crate::types::events::EngineToRuntimeEvent;
use omini_config::{RawConfig, Settings};
use omini_domain::agent_run::{
    AgentRunSnapshot, AgentRunStatus, AgentStepSnapshot, AgentStepStatus, ToolUseExecutionSnapshot,
    ToolUseStatus,
};
use omini_domain::conversation::{AgentMessage, CompactionSummary, ProposedPlan, TaskNotification};
use omini_domain::task::{TaskInfo, TaskStatus};
use omini_domain::usage::Usage;
use omini_model::message::Message;
use omini_permissions::PermissionEngine;
use omini_runtime_contract::thread_domain::{ActiveProfile, AgentTaskResult, DeliveryKey};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc};

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    pub fn new(label: &str) -> Self {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "omini-core-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("test temp directory should be created");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("test fixture parent should be created");
        }
        std::fs::write(&path, contents).expect("test fixture should be written");
        path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn settings(cwd: &Path, image_input: bool) -> Settings {
    let model = if image_input {
        "vision-model"
    } else {
        "text-model"
    };
    let input = if image_input {
        r#"["text", "image"]"#
    } else {
        r#"["text"]"#
    };
    let raw: RawConfig = toml::from_str(&format!(
        r#"
[providers.test]
protocol = "openai"
base_url = "http://127.0.0.1:9"
api_key = "test-key"

[providers.test.models.{model}]
context_window = 256000
thinking = false
input = {input}
"#
    ))
    .expect("test config should parse");
    raw.resolve()
        .expect("test config should resolve")
        .to_settings(Some("test"), Some(model), None, cwd)
        .expect("test settings should build")
}

pub fn tool_context(cwd: &Path, tool_name: &str, image_input: bool) -> ToolExecutionContext {
    let (event_tx, _event_rx) = mpsc::channel::<EngineToRuntimeEvent>(8);
    let pending_tool_pauses: PendingToolPauses = Arc::new(Mutex::new(HashMap::new()));
    ToolExecutionContext {
        tool_use_id: format!("test-{tool_name}"),
        pause_id: format!("test-{tool_name}"),
        tool_name: tool_name.to_string(),
        settings: Arc::new(settings(cwd, image_input)),
        tool_registry: Arc::new(ToolRegistry::new()),
        event_tx,
        pending_tool_pauses: Arc::clone(&pending_tool_pauses),
        permission_engine: Arc::new(PermissionEngine::empty(cwd.to_path_buf())),
        active_profile: ActiveProfile::Main,
        cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        cancel_notify: Arc::new(Notify::new()),
        runtime: None,
    }
}

/// 测试用 AgentHost：默认全部成功，按操作名记录调用，可注入指定失败。
#[derive(Clone, Default)]
pub struct RecordingHost {
    inner: Arc<RecordingHostInner>,
}

#[derive(Default)]
struct RecordingHostInner {
    calls: Mutex<Vec<String>>,
    /// 需要返回 Err 的操作名集合（如 "fail_pending_task_messages"）。
    failures: Mutex<HashMap<String, String>>,
    /// fail_pending_task_messages 返回的结算条数。
    failed_message_count: Mutex<u32>,
    append_pause: Mutex<Option<Arc<OperationPause>>>,
}

pub struct OperationPause {
    entered: Notify,
    released: Notify,
}

impl OperationPause {
    pub async fn wait_entered(&self) {
        self.entered.notified().await;
    }
    pub fn release(&self) {
        self.released.notify_one();
    }
}

impl RecordingHost {
    pub fn pause_append(&self) -> Arc<OperationPause> {
        let pause = Arc::new(OperationPause {
            entered: Notify::new(),
            released: Notify::new(),
        });
        *self
            .inner
            .append_pause
            .lock()
            .expect("append pause lock poisoned") = Some(Arc::clone(&pause));
        pause
    }

    pub fn record_call(&self, name: &str) {
        self.inner
            .calls
            .lock()
            .expect("recording host calls lock poisoned")
            .push(name.to_string());
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner
            .calls
            .lock()
            .expect("recording host calls lock poisoned")
            .clone()
    }

    pub fn call_count(&self, name: &str) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.as_str() == name)
            .count()
    }

    /// 配置 fail_pending_task_messages 返回的结算条数。
    pub fn set_failed_message_count(&self, count: u32) {
        *self
            .inner
            .failed_message_count
            .lock()
            .expect("recording host failure count lock poisoned") = count;
    }

    /// 注入一次指定操作的失败消息。
    pub fn fail_operation(&self, name: &str, message: &str) {
        self.inner
            .failures
            .lock()
            .expect("recording host failures lock poisoned")
            .insert(name.to_string(), message.to_string());
    }

    fn result(&self, name: &str) -> Result<(), HostError> {
        self.record_call(name);
        match self
            .inner
            .failures
            .lock()
            .expect("recording host failures lock poisoned")
            .get(name)
        {
            Some(message) => Err(HostError::new("recording host", message.clone())),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl AgentHost for RecordingHost {
    async fn create_agent_run(&self, _run: &AgentRunSnapshot) -> Result<(), HostError> {
        self.result("create_agent_run")
    }

    async fn update_agent_run(
        &self,
        _run_id: &str,
        _status: AgentRunStatus,
        _started_at: Option<jiff::Timestamp>,
        _finished_at: Option<jiff::Timestamp>,
        _add_tokens: i64,
    ) -> Result<(), HostError> {
        self.result("update_agent_run")
    }

    async fn upsert_agent_step(&self, _step: &AgentStepSnapshot) -> Result<(), HostError> {
        self.result("upsert_agent_step")
    }

    async fn update_agent_step(
        &self,
        _step_id: &str,
        _status: AgentStepStatus,
        _finished_at: Option<jiff::Timestamp>,
        _add_input_tokens: i64,
        _add_output_tokens: i64,
    ) -> Result<(), HostError> {
        self.result("update_agent_step")
    }

    async fn upsert_tool_use_execution(
        &self,
        _tool_use: &ToolUseExecutionSnapshot,
        _status: ToolUseStatus,
    ) -> Result<(), HostError> {
        self.result("upsert_tool_use_execution")
    }

    async fn append_llm_message(
        &self,
        _thread_id: &str,
        _message: &Message,
    ) -> Result<(), HostError> {
        let pause = self
            .inner
            .append_pause
            .lock()
            .expect("append pause lock poisoned")
            .take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.released.notified().await;
        }
        self.result("append_llm_message")
    }

    async fn append_ui_message(
        &self,
        _thread_id: &str,
        _message: &Message,
        _model_ref: Option<&str>,
    ) -> Result<(), HostError> {
        self.result("append_ui_message")
    }

    async fn insert_plan_message(
        &self,
        _thread_id: &str,
        _plan: &ProposedPlan,
        _model_ref: &str,
    ) -> Result<(), HostError> {
        self.result("insert_plan_message")
    }

    async fn insert_compact_summary(
        &self,
        _thread_id: &str,
        _summary: &CompactionSummary,
        _model_ref: &str,
    ) -> Result<(), HostError> {
        self.result("insert_compact_summary")
    }

    async fn replace_llm_context(
        &self,
        _thread_id: &str,
        expected_version: i64,
        _messages: Vec<Message>,
    ) -> Result<i64, HostError> {
        self.result("replace_llm_context")?;
        Ok(expected_version + 1)
    }

    async fn record_thread_usage(&self, _thread_id: &str, _usage: Usage) -> Result<(), HostError> {
        self.result("record_thread_usage")
    }

    async fn record_thread_total_usage(
        &self,
        _thread_id: &str,
        _usage: Usage,
    ) -> Result<(), HostError> {
        self.result("record_thread_total_usage")
    }

    async fn record_owner_agent_usage(
        &self,
        _owner_thread_id: &str,
        _usage: Usage,
    ) -> Result<(), HostError> {
        self.result("record_owner_agent_usage")
    }

    async fn update_thread_config(
        &self,
        _thread_id: &str,
        _provider: &str,
        _model: &str,
        _thinking_effort: Option<&str>,
    ) -> Result<(), HostError> {
        self.result("update_thread_config")
    }

    async fn update_thread_thinking_effort(
        &self,
        _thread_id: &str,
        _thinking_effort: Option<&str>,
    ) -> Result<(), HostError> {
        self.result("update_thread_thinking_effort")
    }

    async fn touch_thread(&self, _thread_id: &str) -> Result<(), HostError> {
        self.result("touch_thread")
    }

    async fn upsert_background_task(&self, _task: &TaskInfo) -> Result<(), HostError> {
        self.result("upsert_background_task")
    }

    async fn insert_task_notification(
        &self,
        _owner_thread_id: &str,
        _notification: &TaskNotification,
        _llm_message: &Message,
        _task_ids: &[String],
    ) -> Result<(), HostError> {
        self.result("insert_task_notification")
    }

    async fn create_agent_session(
        &self,
        request: AgentSessionRequest,
    ) -> Result<AgentSession, HostError> {
        self.result("create_agent_session")?;
        let thread_dir = omini_config::project::ThreadDir::from_path(
            std::env::temp_dir().join(format!("omini-recording-host-{}", request.task_id)),
        );
        Ok(AgentSession {
            thread_id: format!("child-of-{}", request.owner_thread_id),
            thread_dir,
        })
    }

    async fn persist_agent_message(
        &self,
        _agent_thread_id: &str,
        _message: &Message,
        _model_ref: Option<&str>,
        _persist_llm_history: bool,
        _display_in_ui: bool,
    ) -> Result<(), HostError> {
        self.result("persist_agent_message")
    }

    async fn enqueue_agent_message(
        &self,
        _task_id: &str,
        _owner_thread_id: &str,
        _agent_thread_id: &str,
        _message: &AgentMessage,
    ) -> Result<(), HostError> {
        self.result("enqueue_agent_message")
    }

    async fn inject_task_message(
        &self,
        _key: &DeliveryKey,
        _agent_thread_id: &str,
        _model_message: &Message,
    ) -> Result<(), HostError> {
        self.result("inject_task_message")
    }

    async fn fail_pending_task_messages(
        &self,
        _task_id: &str,
        _reason: &str,
    ) -> Result<u32, HostError> {
        self.result("fail_pending_task_messages")?;
        Ok(*self
            .inner
            .failed_message_count
            .lock()
            .expect("recording host failure count lock poisoned"))
    }

    async fn finish_agent_task(
        &self,
        _task_id: &str,
        _status: TaskStatus,
        _result: &AgentTaskResult,
        _completed_at: jiff::Timestamp,
    ) -> Result<(), HostError> {
        self.result("finish_agent_task")
    }

    async fn set_agent_tasks_cancelling(&self, _task_ids: &[String]) -> Result<(), HostError> {
        self.result("set_agent_tasks_cancelling")
    }
}
