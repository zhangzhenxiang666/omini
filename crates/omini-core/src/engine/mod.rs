use crate::error::RuntimeError;
use crate::runtime::compact::AutoCompactState;
use crate::subagents::AgentTaskCompletion;
use crate::tools::{PendingToolPauses, ToolRegistry, ToolRuntimeContext};
use crate::types::events::EngineToRuntimeEvent;
use omini_config::Settings;
use omini_domain::display::{AgentTaskNotification, AgentTaskNotificationItem};
use omini_domain::events::{ActiveProfile, AgentTaskStatus, ToolPauseResponse};
use omini_domain::message::Message;
use omini_permissions::PermissionEngine;
use omini_provider_api::{FinishReason, LlmClient};
use serde::Serialize;
use state::{FinalizationReason, QueryState, REPEAT_LIMIT, RepeatGuard, TurnOutcome};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;
use tool::{ToolExecutor, ToolRunResult};

pub use pause::ToolPauseResolver;

mod pause;
mod state;
mod tool;
mod turn;

#[derive(Debug, Clone)]
pub struct QueryResult {
    /// 已执行的 LLM Turn 数，包含 finalization turn 和请求失败的 turn。
    pub turns: usize,
    /// 最后一轮 LLM 或运行时错误对应的 finish reason。
    pub finish_reason: FinishReason,
    /// 已持久化的内部输入落在终止边界，需要启动一次 continuation。
    pub follow_up: bool,
}

#[derive(Debug)]
pub struct QueryContext<'a> {
    pub messages: &'a mut Vec<Message>,
    pub settings: Arc<Settings>,
    pub llm_client: LlmClient,
    pub tool_registry: Arc<ToolRegistry>,
    pub active_profile: Arc<RwLock<ActiveProfile>>,
    pub runtime_context: Option<Arc<ToolRuntimeContext>>,
    /// 当前 query 只能在成功注入内部输入后调用 provider。
    pub requires_internal_input: bool,
}

pub struct QueryEngine {
    tool_pause_resolver: ToolPauseResolver,
    permission_engine: Arc<PermissionEngine>,
    cancel_notify: Arc<Notify>,
    drain_pauses_on_start: bool,
    pending_user_messages: Mutex<VecDeque<PendingUserMessage>>,
    // Task completion 还需要原子持久化、失败重排队和 delivered 标记，
    // 生命周期不同于一次性的用户干预消息。
    pending_agent_task_completions: Mutex<VecDeque<AgentTaskCompletion>>,
}

struct PendingUserMessage {
    message: Message,
    client_echo_id: Option<String>,
}

impl QueryEngine {
    pub fn new(permission_engine: Arc<PermissionEngine>) -> Self {
        Self {
            tool_pause_resolver: ToolPauseResolver::new(Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            ))),
            permission_engine,
            cancel_notify: Arc::new(Notify::new()),
            drain_pauses_on_start: true,
            pending_user_messages: Mutex::new(VecDeque::new()),
            pending_agent_task_completions: Mutex::new(VecDeque::new()),
        }
    }

    pub fn with_shared_tool_controls(
        pending_tool_pauses: PendingToolPauses,
        permission_engine: Arc<PermissionEngine>,
        cancel_notify: Arc<Notify>,
    ) -> Self {
        Self {
            tool_pause_resolver: ToolPauseResolver::new(pending_tool_pauses),
            permission_engine,
            cancel_notify,
            drain_pauses_on_start: false,
            pending_user_messages: Mutex::new(VecDeque::new()),
            pending_agent_task_completions: Mutex::new(VecDeque::new()),
        }
    }

    /// 将用户干预消息排队，在当前 Turn 收尾后注入历史。
    pub fn enqueue_user_message(&self, message: Message, client_echo_id: Option<String>) {
        let mut pending = self
            .pending_user_messages
            .lock()
            .expect("pending user messages mutex poisoned");
        pending.push_back(PendingUserMessage {
            message,
            client_echo_id,
        });
    }

    pub fn enqueue_agent_task_completion(&self, completion: AgentTaskCompletion) {
        self.pending_agent_task_completions
            .lock()
            .expect("pending agent task completions mutex poisoned")
            .push_back(completion);
    }

    /// 唤醒当前运行中的取消等待点。
    pub fn notify_cancel_waiters(&self) {
        if self.drain_pauses_on_start {
            self.tool_pause_resolver.drain_pending_tool_pauses();
        }
        self.cancel_notify.notify_waiters();
    }

    pub fn resolve_tool_pause(
        &self,
        tool_use_id: &str,
        response: ToolPauseResponse,
    ) -> Result<(), RuntimeError> {
        self.tool_pause_resolver
            .resolve_tool_pause(tool_use_id, response)
    }

    pub fn tool_pause_resolver(&self) -> ToolPauseResolver {
        self.tool_pause_resolver.clone()
    }

    pub fn pending_tool_pauses(&self) -> PendingToolPauses {
        self.tool_pause_resolver.pending_tool_pauses()
    }

    pub fn permission_engine(&self) -> Arc<PermissionEngine> {
        Arc::clone(&self.permission_engine)
    }

    pub fn cancel_notify_arc(&self) -> Arc<Notify> {
        Arc::clone(&self.cancel_notify)
    }

    pub async fn run_query(
        &self,
        mut ctx: QueryContext<'_>,
        event_tx: mpsc::Sender<EngineToRuntimeEvent>,
        cancelled: Arc<AtomicBool>,
    ) -> QueryResult {
        self.tool_pause_resolver.drain_pending_tool_pauses();
        self.clear_pending_user_messages();

        let tool_definitions = ctx.tool_registry.definitions();
        let tool_executor = ToolExecutor::new(
            Arc::clone(&ctx.settings),
            self.tool_pause_resolver.pending_tool_pauses(),
            Arc::clone(&self.permission_engine),
            Arc::clone(&ctx.active_profile),
            Arc::clone(&cancelled),
            Arc::clone(&self.cancel_notify),
            ctx.runtime_context.clone(),
            Arc::clone(&ctx.tool_registry),
            event_tx.clone(),
        );
        let mut tool_tasks = JoinSet::<ToolRunResult>::new();
        let mut compact_state = AutoCompactState::default();
        let mut repeat_guard = RepeatGuard::new(REPEAT_LIMIT);
        let mut state = QueryState::new();
        let mut follow_up = false;
        let mut notification_persistence_failed = false;

        match self
            .drain_agent_task_completions(ctx.messages, &event_tx)
            .await
        {
            AgentTaskNotificationDrain::Injected => {}
            AgentTaskNotificationDrain::Empty if !ctx.requires_internal_input => {}
            AgentTaskNotificationDrain::Empty | AgentTaskNotificationDrain::Failed => {
                notification_persistence_failed = true;
                if ctx.requires_internal_input {
                    let mut result = state.into_result();
                    result.finish_reason = FinishReason::Error(
                        "agent task notification could not be persisted".to_string(),
                    );
                    return result;
                }
            }
        }

        loop {
            if state.turn_limit_reached(ctx.settings.max_turns) {
                state.finalize(FinalizationReason::MaxTurnsReached);
            }

            if cancelled.load(Ordering::Relaxed) {
                state.mark_cancelled();
                break;
            }

            debug_assert!(
                tool_tasks.is_empty(),
                "previous Turn left tool tasks behind"
            );

            let mode = state.turn_mode();
            let outcome = self
                .execute_turn(
                    &mut ctx,
                    &event_tx,
                    &cancelled,
                    &tool_definitions,
                    &tool_executor,
                    &mut tool_tasks,
                    &mut repeat_guard,
                    &mut compact_state,
                    mode,
                    state.turns(),
                )
                .await;

            state.record_turn(outcome.finish_reason().clone());

            let TurnOutcome::Completed {
                finish_reason,
                requested_finalization,
                stop_after_permission_denial,
            } = outcome
            else {
                break;
            };

            let task_notification = self
                .drain_agent_task_completions(ctx.messages, &event_tx)
                .await;
            let had_task_notification = task_notification == AgentTaskNotificationDrain::Injected;
            notification_persistence_failed |=
                task_notification == AgentTaskNotificationDrain::Failed;

            if had_task_notification && mode.is_finalization() {
                follow_up = true;
            }

            // Finalization 是一次性阶段，不再由 provider 的 finish_reason 驱动循环。
            if mode.is_finalization() {
                break;
            }

            let had_intervention = self
                .drain_pending_user_messages(ctx.messages, &event_tx)
                .await;

            if let Some(reason) = requested_finalization {
                state.finalize(reason);
                continue;
            }

            if stop_after_permission_denial && !had_intervention && !had_task_notification {
                break;
            }

            if !had_intervention
                && !had_task_notification
                && !matches!(finish_reason, FinishReason::ToolUse)
            {
                break;
            }
        }

        debug_assert!(tool_tasks.is_empty(), "Query ended with live tool tasks");
        self.tool_pause_resolver.drain_pending_tool_pauses();
        self.clear_pending_user_messages();
        let has_pending_notification = !self
            .pending_agent_task_completions
            .lock()
            .expect("pending agent task completions mutex poisoned")
            .is_empty();
        let mut result = state.into_result();
        if has_pending_notification
            && !notification_persistence_failed
            && !matches!(result.finish_reason, FinishReason::Error(_))
        {
            follow_up = true;
        }
        result.follow_up = follow_up;
        result
    }

    fn clear_pending_user_messages(&self) {
        self.pending_user_messages
            .lock()
            .expect("pending user messages mutex poisoned")
            .clear();
    }

    async fn drain_pending_user_messages(
        &self,
        messages: &mut Vec<Message>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> bool {
        let pending = self
            .pending_user_messages
            .lock()
            .expect("pending user messages mutex poisoned")
            .drain(..)
            .collect::<Vec<_>>();

        let injected = !pending.is_empty();
        for pending in pending {
            messages.push(pending.message.clone());
            let _ = event_tx
                .send(EngineToRuntimeEvent::UserMessageProduced {
                    message: pending.message,
                    client_echo_id: pending.client_echo_id,
                })
                .await;
        }
        injected
    }

    async fn drain_agent_task_completions(
        &self,
        messages: &mut Vec<Message>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> AgentTaskNotificationDrain {
        let completions = self
            .pending_agent_task_completions
            .lock()
            .expect("pending agent task completions mutex poisoned")
            .drain(..)
            .collect::<Vec<_>>();
        if completions.is_empty() {
            return AgentTaskNotificationDrain::Empty;
        }

        let notification = AgentTaskNotification {
            tasks: completions
                .iter()
                .map(|completion| AgentTaskNotificationItem {
                    task_id: completion.task_id.clone(),
                    agent: completion.agent.clone(),
                    title: completion.title.clone(),
                    status: completion.status,
                })
                .collect(),
            created_at: chrono::Utc::now(),
        };
        let llm_tasks = completions
            .iter()
            .map(|completion| AgentTaskCompletionNotification {
                task_id: &completion.task_id,
                status: completion.status,
            })
            .collect::<Vec<_>>();
        let llm_message = Message::from_user_text(format!(
            "<agent_task_notifications>{}</agent_task_notifications>",
            serde_json::to_string(&llm_tasks).unwrap_or_else(|_| "[]".to_string())
        ));
        let task_ids = completions
            .iter()
            .map(|completion| completion.task_id.clone())
            .collect::<Vec<_>>();
        let (ack, result) = tokio::sync::oneshot::channel();
        let persisted = if event_tx
            .send(EngineToRuntimeEvent::AgentTaskNotificationsProduced {
                notification,
                llm_message: llm_message.clone(),
                task_ids,
                ack,
            })
            .await
            .is_err()
        {
            Err("runtime event processor closed".to_string())
        } else {
            result
                .await
                .map_err(|_| "persistence acknowledgement dropped".to_string())
                .and_then(|result| result)
        };

        match persisted {
            Ok(()) => {
                messages.push(llm_message);
                AgentTaskNotificationDrain::Injected
            }
            Err(error) => {
                {
                    let mut pending = self
                        .pending_agent_task_completions
                        .lock()
                        .expect("pending agent task completions mutex poisoned");
                    for completion in completions.into_iter().rev() {
                        pending.push_front(completion);
                    }
                }
                let _ = event_tx
                    .send(EngineToRuntimeEvent::Warning(format!(
                        "Failed to persist agent task notification: {error}"
                    )))
                    .await;
                AgentTaskNotificationDrain::Failed
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentTaskNotificationDrain {
    Empty,
    Injected,
    Failed,
}

#[derive(Serialize)]
struct AgentTaskCompletionNotification<'a> {
    task_id: &'a str,
    status: AgentTaskStatus,
}

impl Default for QueryEngine {
    fn default() -> Self {
        Self::new(Arc::new(PermissionEngine::empty(
            std::env::current_dir().unwrap_or_else(|_| ".".into()),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagents::AgentTaskCompletion;
    use crate::tools::ToolRegistry;
    use omini_config::{CompactConfig, ModelTiers};
    use omini_domain::config::ProviderEndpointKind;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::OnceLock;
    use std::sync::mpsc as std_mpsc;
    use std::thread;
    use std::time::Duration;

    fn test_settings() -> Settings {
        Settings {
            api_key: "test-key".to_string(),
            base_url: url::Url::parse("http://127.0.0.1:9").unwrap(),
            model: "test-model".to_string(),
            endpoint: ProviderEndpointKind::OpenAI,
            providers: HashMap::new(),
            active_provider: "test".to_string(),
            system_prompt: None,
            language: None,
            max_turns: Some(1),
            cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
            thinking_effort: None,
            permissions: None,
            compact: CompactConfig {
                enabled: false,
                ..CompactConfig::default()
            },
            mcp_servers: HashMap::new(),
            model_tiers: ModelTiers::default(),
        }
    }

    fn test_http_client() -> &'static reqwest::Client {
        static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
        CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("test HTTP client should build")
        })
    }

    fn test_llm_client(base_url: String) -> LlmClient {
        LlmClient::with_http_client(
            ProviderEndpointKind::OpenAI,
            "test-key".to_string(),
            url::Url::parse(&base_url).unwrap(),
            test_http_client(),
        )
    }

    fn task_completion(task_id: &str) -> AgentTaskCompletion {
        AgentTaskCompletion {
            task_id: task_id.to_string(),
            agent: "general".to_string(),
            title: format!("Task {task_id}"),
            status: omini_domain::events::AgentTaskStatus::Completed,
        }
    }

    #[tokio::test]
    async fn task_notifications_are_batched_and_enter_history_only_after_ack() {
        let engine = QueryEngine::default();
        engine.enqueue_agent_task_completion(task_completion("task_1"));
        engine.enqueue_agent_task_completion(task_completion("task_2"));
        let (event_tx, mut event_rx) = mpsc::channel(1);
        let observer = tokio::spawn(async move {
            let EngineToRuntimeEvent::AgentTaskNotificationsProduced {
                notification,
                llm_message,
                task_ids,
                ack,
                ..
            } = event_rx.recv().await.unwrap()
            else {
                panic!("expected agent task notification event");
            };
            assert_eq!(task_ids, ["task_1", "task_2"]);
            assert_eq!(notification.tasks.len(), 2);
            let omini_domain::message::ContentBlock::Text(text) = &llm_message.content[0] else {
                panic!("task notification should be a text message");
            };
            let payload = text
                .text
                .strip_prefix("<agent_task_notifications>")
                .and_then(|text| text.strip_suffix("</agent_task_notifications>"))
                .expect("task notification envelope");
            let tasks: serde_json::Value = serde_json::from_str(payload).unwrap();
            assert_eq!(tasks[0]["task_id"], "task_1");
            assert_eq!(tasks[0]["status"], "completed");
            assert_eq!(tasks[0].as_object().unwrap().len(), 2);
            ack.send(Ok(())).unwrap();
        });
        let mut messages = vec![Message::from_user_text("before".to_string())];

        let outcome = engine
            .drain_agent_task_completions(&mut messages, &event_tx)
            .await;

        observer.await.unwrap();
        assert_eq!(outcome, AgentTaskNotificationDrain::Injected);
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages[1].content.as_slice(),
            [omini_domain::message::ContentBlock::Text(text)]
                if text.text.contains("agent_task_notifications")
        ));
    }

    #[tokio::test]
    async fn failed_task_notification_persistence_keeps_queue_and_history_unchanged() {
        let engine = QueryEngine::default();
        engine.enqueue_agent_task_completion(task_completion("task_1"));
        let (event_tx, mut event_rx) = mpsc::channel(2);
        let observer = tokio::spawn(async move {
            let EngineToRuntimeEvent::AgentTaskNotificationsProduced { ack, .. } =
                event_rx.recv().await.unwrap()
            else {
                panic!("expected agent task notification event");
            };
            ack.send(Err("database unavailable".to_string())).unwrap();
            assert!(matches!(
                event_rx.recv().await,
                Some(EngineToRuntimeEvent::Warning(_))
            ));
        });
        let mut messages = vec![Message::from_user_text("before".to_string())];

        let outcome = engine
            .drain_agent_task_completions(&mut messages, &event_tx)
            .await;

        observer.await.unwrap();
        assert_eq!(outcome, AgentTaskNotificationDrain::Failed);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            engine
                .pending_agent_task_completions
                .lock()
                .expect("pending completions mutex poisoned")
                .len(),
            1
        );
    }

    fn spawn_hanging_openai_server() -> (String, std_mpsc::Receiver<()>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("test server addr")
        );
        let (accepted_tx, accepted_rx) = std_mpsc::channel();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = accepted_tx.send(());

            let mut buf = [0_u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            || error.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        break;
                    }
                    Err(_) => break,
                }
            }
        });

        (base_url, accepted_rx, handle)
    }

    fn spawn_retryable_openai_server() -> (String, std_mpsc::Receiver<()>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("test server addr")
        );
        let (responded_tx, responded_rx) = std_mpsc::channel();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror",
                )
                .expect("write retryable response");
            let _ = stream.flush();
            let _ = responded_tx.send(());
        });

        (base_url, responded_rx, handle)
    }

    fn spawn_openai_stop_server(requests: usize) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("test server addr")
        );
        let handle = thread::spawn(move || {
            for index in 0..requests {
                let (mut stream, _) = listener.accept().expect("accept test request");
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0_u8; 8192];
                let _ = stream.read(&mut request);
                let body = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"answer {index}\"}},\"finish_reason\":null}}]}}\n\n\
                     data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1}}}}\n\n\
                     data: [DONE]\n\n"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write streaming response");
                stream.flush().expect("flush streaming response");
            }
        });
        (base_url, handle)
    }

    fn spawn_openai_repeated_tool_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("test server addr")
        );
        let handle = thread::spawn(move || {
            for index in 0..=REPEAT_LIMIT {
                let (mut stream, _) = listener.accept().expect("accept test request");
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut request = [0_u8; 8192];
                let _ = stream.read(&mut request);
                let body = if index < REPEAT_LIMIT {
                    let tool_delta = serde_json::json!({
                        "choices": [{
                            "delta": {
                                "tool_calls": [{
                                    "index": 0,
                                    "id": format!("call_{index}"),
                                    "type": "function",
                                    "function": {
                                        "name": "missing",
                                        "arguments": "{}"
                                    }
                                }]
                            },
                            "finish_reason": null
                        }]
                    });
                    let done = serde_json::json!({
                        "choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                    });
                    format!("data: {tool_delta}\n\ndata: {done}\n\ndata: [DONE]\n\n")
                } else {
                    let delta = serde_json::json!({
                        "choices": [{
                            "delta": {"content": "finalized"},
                            "finish_reason": null
                        }]
                    });
                    let done = serde_json::json!({
                        "choices": [{"delta": {}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                    });
                    format!("data: {delta}\n\ndata: {done}\n\ndata: [DONE]\n\n")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write streaming response");
                stream.flush().expect("flush streaming response");
            }
        });
        (base_url, handle)
    }

    async fn run_query_with_completion_during_first_turn(
        max_turns: Option<usize>,
        expected_requests: usize,
    ) -> (QueryResult, Vec<Message>, usize) {
        let (base_url, server) = spawn_openai_stop_server(expected_requests);
        let engine = Arc::new(QueryEngine::default());
        let observer_engine = Arc::clone(&engine);
        let (event_tx, mut event_rx) = mpsc::channel(1);
        let observer = tokio::spawn(async move {
            let mut assistant_messages = 0;
            let mut notifications = 0;
            while let Some(event) = event_rx.recv().await {
                match event {
                    EngineToRuntimeEvent::MessageProduced(_) => {
                        assistant_messages += 1;
                        if assistant_messages == 1 {
                            observer_engine
                                .enqueue_agent_task_completion(task_completion("task_1"));
                        }
                    }
                    EngineToRuntimeEvent::AgentTaskNotificationsProduced { ack, .. } => {
                        notifications += 1;
                        ack.send(Ok(())).unwrap();
                    }
                    _ => {}
                }
            }
            notifications
        });
        let mut settings = test_settings();
        settings.max_turns = max_turns;
        let mut messages = vec![Message::from_user_text("hello".to_string())];
        let result = engine
            .run_query(
                QueryContext {
                    messages: &mut messages,
                    settings: Arc::new(settings),
                    llm_client: test_llm_client(base_url),
                    tool_registry: Arc::new(ToolRegistry::new()),
                    active_profile: Arc::new(RwLock::new(ActiveProfile::Main)),
                    runtime_context: None,
                    requires_internal_input: false,
                },
                event_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        let notifications = observer.await.unwrap();
        server.join().expect("test server should exit");
        (result, messages, notifications)
    }

    #[tokio::test]
    async fn completion_at_normal_turn_boundary_continues_current_query() {
        let (result, messages, notifications) =
            run_query_with_completion_during_first_turn(None, 2).await;

        assert_eq!(result.turns, 2);
        assert!(!result.follow_up);
        assert_eq!(notifications, 1);
        assert_eq!(messages.len(), 4);
    }

    #[tokio::test]
    async fn completion_at_finalization_boundary_requests_one_follow_up() {
        let (result, messages, notifications) =
            run_query_with_completion_during_first_turn(Some(0), 1).await;

        assert_eq!(result.turns, 1);
        assert!(result.follow_up);
        assert_eq!(notifications, 1);
        assert_eq!(messages.len(), 3);
    }

    #[tokio::test]
    async fn completion_before_requested_finalization_is_consumed_without_follow_up() {
        let (base_url, server) = spawn_openai_repeated_tool_server();
        let engine = Arc::new(QueryEngine::default());
        let observer_engine = Arc::clone(&engine);
        let (event_tx, mut event_rx) = mpsc::channel(1);
        let observer = tokio::spawn(async move {
            let mut tool_uses = 0;
            let mut notifications = 0;
            while let Some(event) = event_rx.recv().await {
                match event {
                    EngineToRuntimeEvent::ToolUse(_) => {
                        tool_uses += 1;
                        if tool_uses == REPEAT_LIMIT {
                            observer_engine
                                .enqueue_agent_task_completion(task_completion("task_1"));
                        }
                    }
                    EngineToRuntimeEvent::AgentTaskNotificationsProduced { ack, .. } => {
                        notifications += 1;
                        ack.send(Ok(())).unwrap();
                    }
                    _ => {}
                }
            }
            (notifications, tool_uses)
        });
        let mut messages = vec![Message::from_user_text("hello".to_string())];
        let mut settings = test_settings();
        settings.max_turns = None;
        let result = engine
            .run_query(
                QueryContext {
                    messages: &mut messages,
                    settings: Arc::new(settings),
                    llm_client: test_llm_client(base_url),
                    tool_registry: Arc::new(ToolRegistry::new()),
                    active_profile: Arc::new(RwLock::new(ActiveProfile::Main)),
                    runtime_context: None,
                    requires_internal_input: false,
                },
                event_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;

        let (notifications, tool_uses) = observer.await.unwrap();
        server.join().expect("test server should exit");
        assert_eq!(tool_uses, REPEAT_LIMIT, "query result: {result:?}");
        assert_eq!(notifications, 1);
        assert_eq!(result.turns, REPEAT_LIMIT + 1);
        assert!(!result.follow_up);
        assert!(matches!(result.finish_reason, FinishReason::Stop));
    }

    async fn wait_for_server_signal(
        signal_rx: std_mpsc::Receiver<()>,
    ) -> Result<(), std_mpsc::RecvTimeoutError> {
        tokio::task::spawn_blocking(move || signal_rx.recv_timeout(Duration::from_secs(2)))
            .await
            .expect("server signal task should not panic")
    }

    async fn cancel_query_after_server_signal(
        base_url: String,
        signal_rx: std_mpsc::Receiver<()>,
        delay_before_cancel: Option<Duration>,
    ) -> (QueryResult, Vec<EngineToRuntimeEvent>) {
        let engine = QueryEngine::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = mpsc::channel(16);
        let mut messages = vec![Message::from_user_text("hello".to_string())];
        let settings = Arc::new(test_settings());
        let tool_registry = Arc::new(ToolRegistry::new());
        let llm_client = test_llm_client(base_url);
        let ctx = QueryContext {
            messages: &mut messages,
            settings,
            llm_client,
            tool_registry,
            active_profile: Arc::new(RwLock::new(ActiveProfile::Main)),
            runtime_context: None,
            requires_internal_input: false,
        };

        let query = engine.run_query(ctx, tx, Arc::clone(&cancelled));
        tokio::pin!(query);
        let server_signal = wait_for_server_signal(signal_rx);
        tokio::pin!(server_signal);

        tokio::select! {
            signal = &mut server_signal => {
                signal.expect("test server should receive the request");
            }
            result = &mut query => {
                panic!("query finished before cancellation: {:?}", result.finish_reason);
            }
        }

        if let Some(delay) = delay_before_cancel {
            tokio::time::sleep(delay).await;
        }
        cancelled.store(true, Ordering::Relaxed);
        engine.notify_cancel_waiters();

        let result = tokio::time::timeout(Duration::from_millis(500), &mut query)
            .await
            .expect("query should return promptly after cancellation");
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        (result, events)
    }

    #[tokio::test]
    async fn request_phase_cancel_drops_in_flight_http_connect() {
        let (base_url, accepted_rx, server) = spawn_hanging_openai_server();

        let (result, events) = cancel_query_after_server_signal(base_url, accepted_rx, None).await;

        server.join().expect("test server should exit");
        assert_eq!(result.turns, 1);
        assert!(matches!(
            result.finish_reason,
            FinishReason::Error(ref error) if error == "Cancelled"
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, EngineToRuntimeEvent::TurnStarted))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, EngineToRuntimeEvent::TurnEnded))
        );
    }

    #[tokio::test]
    async fn request_phase_cancel_interrupts_retry_backoff() {
        let (base_url, responded_rx, server) = spawn_retryable_openai_server();

        let (result, events) = cancel_query_after_server_signal(
            base_url,
            responded_rx,
            Some(Duration::from_millis(50)),
        )
        .await;

        server.join().expect("test server should exit");
        assert_eq!(result.turns, 1);
        assert!(matches!(
            result.finish_reason,
            FinishReason::Error(ref error) if error == "Cancelled"
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, EngineToRuntimeEvent::TurnEnded))
        );
    }
}
