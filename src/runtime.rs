use crate::api::LlmClient;
use crate::engine::{QueryContext, QueryEngine};
use crate::types::config::Settings;
use crate::types::events::{RuntimeEvent, UiRequest};
use crate::types::message::Message;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Agent 运行时。
///
/// 维护自己的对话历史，通过 channel 与 UI 双向通信。
/// 一次 `SendMessage` 可能触发多轮 LLM 调用 + 工具执行，
/// 直到 LLM 自然结束或达到最大轮次。
pub struct AgentRuntime {
    /// 当前会话 ID（第一次提交时生成）
    session_id: Option<String>,
    /// 向 UI 发送事件
    event_tx: mpsc::Sender<RuntimeEvent>,
    /// 接收 UI 发来的请求
    request_rx: mpsc::Receiver<UiRequest>,
    /// 配置
    settings: Settings,
    /// 运行时自主维护的对话历史
    messages: Vec<Message>,
    /// LLM 客户端
    llm_client: LlmClient,
    /// 查询引擎
    query_engine: QueryEngine,
    /// 取消标志（用于 CancelRun）
    cancelled: Arc<AtomicBool>,
}

impl AgentRuntime {
    pub fn new(
        event_tx: mpsc::Sender<RuntimeEvent>,
        request_rx: mpsc::Receiver<UiRequest>,
        settings: Settings,
    ) -> Self {
        let llm_client = LlmClient::new(
            settings.endpoint,
            settings.api_key.clone(),
            settings.base_url.clone(),
        );
        Self {
            session_id: None,
            event_tx,
            request_rx,
            settings,
            messages: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            llm_client,
            query_engine: QueryEngine::new(),
        }
    }

    /// 启动运行时，返回 JoinHandle。
    ///
    /// runtime 在内部 task 中运行，UI 可通过 handle 在退出时 join。
    pub fn run(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            UiRequest::SendMessage(text) => {
                                self.messages.push(Message::from_user_text(text));
                                self.process_run().await;
                            }
                            UiRequest::CancelRun => {
                                self.cancelled.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                    // request_rx 断开 → 退出
                    else => break,
                }
            }
        })
    }

    /// 处理一次完整的用户请求（可能含多轮 LLM 调用）。
    async fn process_run(&mut self) {
        // 第一次提交时创建 session
        if self.session_id.is_none() {
            self.session_id = Some(Uuid::new_v4().to_string());
        }

        self.send_event(RuntimeEvent::RunStarted).await;

        // 构造 QueryContext 并委托给 QueryEngine
        let ctx = QueryContext {
            messages: &mut self.messages,
            settings: &self.settings,
            llm_client: &self.llm_client,
        };

        let _result = self
            .query_engine
            .run_query(ctx, self.event_tx.clone(), Arc::clone(&self.cancelled))
            .await;

        self.cancelled.store(false, Ordering::Relaxed);
        self.send_event(RuntimeEvent::RunFinished).await;
    }

    /// 发送事件到 UI（忽略 send 失败）
    async fn send_event(&self, event: RuntimeEvent) {
        let _ = self.event_tx.send(event).await;
    }
}
