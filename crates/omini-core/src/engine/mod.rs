use crate::error::RuntimeError;
use crate::runtime::compact::AutoCompactState;
use crate::tools::{PendingToolPauses, ToolRegistry, ToolRuntimeContext};
use crate::types::events::EngineToRuntimeEvent;
use omini_config::Settings;
use omini_domain::events::{ActiveProfile, ToolPauseResponse};
use omini_domain::message::Message;
use omini_permissions::PermissionEngine;
use omini_provider_api::{FinishReason, LlmClient};
use state::{FinalizationReason, QueryState, REPEAT_LIMIT, RepeatGuard, TurnOutcome};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
}

#[derive(Debug)]
pub struct QueryContext<'a> {
    pub messages: &'a mut Vec<Message>,
    pub settings: Arc<Settings>,
    pub llm_client: LlmClient,
    pub tool_registry: Arc<ToolRegistry>,
    pub active_profile: ActiveProfile,
    pub runtime_context: Option<Arc<ToolRuntimeContext>>,
}

pub struct QueryEngine {
    tool_pause_resolver: ToolPauseResolver,
    permission_engine: Arc<PermissionEngine>,
    cancel_notify: Arc<Notify>,
    pending_user_messages: Mutex<VecDeque<PendingUserMessage>>,
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
            pending_user_messages: Mutex::new(VecDeque::new()),
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
            pending_user_messages: Mutex::new(VecDeque::new()),
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

    /// 唤醒当前运行中的取消等待点。
    pub fn notify_cancel_waiters(&self) {
        self.tool_pause_resolver.drain_pending_tool_pauses();
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
            ctx.active_profile,
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

            if stop_after_permission_denial && !had_intervention {
                break;
            }

            if !had_intervention && !matches!(finish_reason, FinishReason::ToolUse) {
                break;
            }
        }

        debug_assert!(tool_tasks.is_empty(), "Query ended with live tool tasks");
        self.tool_pause_resolver.drain_pending_tool_pauses();
        self.clear_pending_user_messages();
        state.into_result()
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
            base_url: String::new(),
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
            base_url,
            test_http_client(),
        )
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
            active_profile: ActiveProfile::Main,
            runtime_context: None,
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
