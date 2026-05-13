use crate::api::{ApiRequest, FinishReason, LlmClient};
use crate::tools::{
    DefaultPermissionPolicy, PendingToolPause, PendingToolPauses, PermissionPolicy,
    ToolExecutionContext, ToolRegistry, ToolResult,
};
use crate::types::config::Settings;
use crate::types::events::{EngineToRuntimeEvent, ToolPauseResponse};
use crate::types::message::{
    ContentBlock, Message, Role, TextBlock, ThinkingBlock, ToolResultBlock, ToolUseBlock,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;
use tokio_stream::StreamExt;

/// 一次查询执行后的结果摘要。
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// 本轮查询总共生成的 assistant 消息数（= 轮次）
    pub turns: usize,
    /// 最终的 finish_reason（最后一轮 LLM 返回的原因）
    pub finish_reason: FinishReason,
    /// 本轮是否有工具调用被执行
    pub had_tool_use: bool,
}

enum ToolCollection {
    Completed(Vec<ToolResultBlock>),
    Cancelled(Vec<ToolResultBlock>),
}

/// 一次查询的上下文。
///
/// 由 `AgentRuntime` 在每次 `process_run` 时构造传入。
/// `messages` 是引擎本地的工作副本，runtime 通过 `EngineToRuntimeEvent` 获取新消息。
#[derive(Debug)]
pub struct QueryContext<'a> {
    pub messages: &'a mut Vec<Message>,
    pub settings: &'a Settings,
    pub llm_client: LlmClient,
    pub tool_registry: Arc<ToolRegistry>,
}

/// 查询引擎。
pub struct QueryEngine {
    pending_tool_pauses: PendingToolPauses,
    permission_policy: Arc<dyn PermissionPolicy>,
    cancel_notify: Arc<Notify>,
    pending_user_messages: Mutex<VecDeque<Message>>,
}

impl QueryEngine {
    /// 创建新的查询引擎。
    pub fn new() -> Self {
        Self {
            pending_tool_pauses: Arc::new(Mutex::new(HashMap::new())),
            permission_policy: Arc::new(DefaultPermissionPolicy),
            cancel_notify: Arc::new(Notify::new()),
            pending_user_messages: Mutex::new(VecDeque::new()),
        }
    }

    /// 将用户干预消息排队，等待当前轮结束后、下一轮 LLM 调用前插入历史。
    pub fn enqueue_user_message(&self, msg: Message) {
        let mut pending = self
            .pending_user_messages
            .lock()
            .expect("pending user messages mutex poisoned");
        pending.push_back(msg);
    }

    fn clear_pending_user_messages(&self) {
        let mut pending = self
            .pending_user_messages
            .lock()
            .expect("pending user messages mutex poisoned");
        pending.clear();
    }

    /// 通知当前 query 取消，唤醒权限等待和工具收集逻辑。
    pub fn cancel_current_run(&self) {
        self.drain_pending_tool_pauses();
        self.cancel_notify.notify_waiters();
    }

    /// 用户响应工具暂停请求。
    pub fn resolve_tool_pause(
        &self,
        tool_use_id: &str,
        response: ToolPauseResponse,
    ) -> Result<(), String> {
        let waiter = {
            let mut pending = self
                .pending_tool_pauses
                .lock()
                .expect("pending tool pause mutex poisoned");
            pending.remove(tool_use_id)
        };

        match (waiter, response) {
            (
                Some(PendingToolPause::Permission(tx)),
                response @ ToolPauseResponse::Permission { .. },
            )
            | (Some(PendingToolPause::Permission(tx)), response @ ToolPauseResponse::Cancelled) => {
                tx.send(response)
                    .map_err(|_| format!("Tool pause waiter closed: {tool_use_id}"))
            }
            (
                Some(PendingToolPause::UserInput(tx)),
                response @ ToolPauseResponse::UserInput { .. },
            )
            | (Some(PendingToolPause::UserInput(tx)), response @ ToolPauseResponse::Cancelled) => {
                tx.send(response)
                    .map_err(|_| format!("Tool pause waiter closed: {tool_use_id}"))
            }
            (Some(_), _) => Err(format!("Tool pause response type mismatch: {tool_use_id}")),
            (None, _) => Err(format!("Unknown tool pause: {tool_use_id}")),
        }
    }

    fn drain_pending_tool_pauses(&self) {
        let waiters: Vec<PendingToolPause> = {
            let mut pending = self
                .pending_tool_pauses
                .lock()
                .expect("pending tool pause mutex poisoned");
            pending.drain().map(|(_, waiter)| waiter).collect()
        };

        for waiter in waiters {
            match waiter {
                PendingToolPause::Permission(tx) | PendingToolPause::UserInput(tx) => {
                    let _ = tx.send(ToolPauseResponse::Cancelled);
                }
            }
        }
    }

    /// 执行一次完整的查询（可能包含多轮 LLM 调用 + 工具执行）。
    ///
    /// # 参数
    /// - `ctx`: 查询上下文（messages、settings、llm_client）
    /// - `event_tx`: 向 Runtime 发送事件的 channel
    /// - `cancelled`: 取消标志（来自 `AgentRuntime`）
    ///
    /// # 返回
    /// `QueryResult` 包含执行摘要。
    pub async fn run_query(
        &self,
        ctx: QueryContext<'_>,
        event_tx: mpsc::Sender<EngineToRuntimeEvent>,
        cancelled: Arc<AtomicBool>,
    ) -> QueryResult {
        self.drain_pending_tool_pauses();
        self.clear_pending_user_messages();
        let max_turns = ctx.settings.max_turns.unwrap_or(200);
        let mut turns = 0;
        let mut finish_reason = FinishReason::Stop;
        let mut had_tool_use = false;
        // 在整个 query 生命周期内复用同一个 JoinSet，每轮 drain 后自动清空
        let mut tool_tasks: JoinSet<ToolResultBlock> = JoinSet::new();
        // 工具定义在一次 process_run 中不会变化，预先计算一次避免重复 clone
        let tool_definitions = ctx.tool_registry.definitions();

        for _turn in 0..max_turns {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }

            let _ = event_tx.send(EngineToRuntimeEvent::TurnStarted).await;

            let request = ApiRequest {
                messages: ctx.messages,
                model: &ctx.settings.model,
                system_prompt: ctx.settings.system_prompt.as_deref(),
                tools: Some(&tool_definitions),
                max_tokens: None,
                temperature: None,
                thinking_effort: ctx.settings.thinking_effort,
            };

            let mut stream = match ctx.llm_client.invoke(request).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::Error(format!(
                            "LLM request failed: {e}"
                        )))
                        .await;
                    break;
                }
            };

            let mut stream_completion = None;
            let mut partial_blocks: Vec<ContentBlock> = Vec::new();
            let mut query_cancelled = false;

            loop {
                let next_event = tokio::select! {
                    event = stream.next() => event,
                    _ = self.cancel_notify.notified() => {
                        if cancelled.load(Ordering::Relaxed) {
                            query_cancelled = true;
                            break;
                        }
                        continue;
                    }
                };

                let Some(event) = next_event else {
                    break;
                };

                if cancelled.load(Ordering::Relaxed) {
                    query_cancelled = true;
                    break;
                }

                match event {
                    Ok(api_event) => match api_event {
                        crate::api::ApiEvent::Text(delta) => {
                            Self::push_text_delta(&mut partial_blocks, &delta);
                            let _ = event_tx.send(EngineToRuntimeEvent::TextDelta(delta)).await;
                        }
                        crate::api::ApiEvent::Thinking(delta) => {
                            Self::push_thinking_delta(&mut partial_blocks, &delta);
                            let _ = event_tx
                                .send(EngineToRuntimeEvent::ThinkingDelta(delta))
                                .await;
                        }
                        crate::api::ApiEvent::ToolUse(tool_use) => {
                            partial_blocks.push(ContentBlock::ToolUse(tool_use.clone()));
                            // 通知 UI：tool 开始执行
                            let _ = event_tx
                                .send(EngineToRuntimeEvent::ToolUse(tool_use.clone()))
                                .await;

                            // 立即在后台 spawn 执行 tool
                            let tx = event_tx.clone();
                            let cancelled = cancelled.clone();
                            let tool_registry = ctx.tool_registry.clone();
                            let pending_tool_pauses = Arc::clone(&self.pending_tool_pauses);
                            let permission_policy = Arc::clone(&self.permission_policy);
                            tool_tasks.spawn(async move {
                                Self::execute_tool(
                                    &tool_registry,
                                    &tool_use,
                                    &tx,
                                    pending_tool_pauses,
                                    permission_policy,
                                    cancelled,
                                )
                                .await
                            });
                        }
                        crate::api::ApiEvent::Done(completion) => {
                            stream_completion = Some(completion);
                        }
                    },
                    Err(stream_err) => {
                        let _ = event_tx
                            .send(EngineToRuntimeEvent::Error(format!(
                                "Stream error: {stream_err}"
                            )))
                            .await;
                        break;
                    }
                }
            }

            if query_cancelled || cancelled.load(Ordering::Relaxed) {
                finish_reason = FinishReason::Error("Cancelled".to_string());
                if !partial_blocks.is_empty() {
                    let msg = Message::new(Role::Assistant, partial_blocks);
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::MessageProduced(msg.clone()))
                        .await;
                    ctx.messages.push(msg);

                    self.drain_pending_tool_pauses();
                    let tool_results = Self::cancel_and_collect_tool_results(
                        &mut tool_tasks,
                        ctx.messages.last().expect("assistant message just pushed"),
                        &event_tx,
                    )
                    .await;

                    if !tool_results.is_empty() {
                        had_tool_use = true;
                        let tool_msg = Message::new(
                            Role::User,
                            tool_results
                                .into_iter()
                                .map(ContentBlock::ToolResult)
                                .collect(),
                        );
                        let _ = event_tx
                            .send(EngineToRuntimeEvent::ToolResultsProduced(tool_msg.clone()))
                            .await;
                        ctx.messages.push(tool_msg);
                    }
                } else {
                    self.drain_pending_tool_pauses();
                    tool_tasks.abort_all();
                    while tool_tasks.join_next().await.is_some() {}
                }

                let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                turns += 1;
                break;
            }

            // 检查 stream 是否正常结束
            let completion = match stream_completion {
                Some(c) => c,
                None => {
                    if !cancelled.load(Ordering::Relaxed) {
                        let _ = event_tx
                            .send(EngineToRuntimeEvent::Error(
                                "Stream ended unexpectedly".into(),
                            ))
                            .await;
                    }
                    break;
                }
            };

            // TODO: 需要将token信息同步(占位)
            finish_reason = completion.finish_reason.clone();

            let msg = completion.message;
            let _ = event_tx
                .send(EngineToRuntimeEvent::MessageProduced(msg.clone()))
                .await;
            ctx.messages.push(msg);

            // JoinSet 按完成顺序返回，但 tool_result 需要与 assistant 消息中的 tool_use 顺序一致
            // 因此用 tool_use_id 建立查找表来重建顺序
            let tool_results = match Self::collect_finished_tool_results(
                &mut tool_tasks,
                &event_tx,
                &cancelled,
                &self.cancel_notify,
            )
            .await
            {
                ToolCollection::Completed(results) => results,
                ToolCollection::Cancelled(results) => {
                    let results = Self::fill_cancelled_tool_results(
                        ctx.messages.last().expect("assistant message just pushed"),
                        results,
                        &event_tx,
                    )
                    .await;
                    had_tool_use = !results.is_empty();
                    if !results.is_empty() {
                        let tool_msg = Message::new(
                            Role::User,
                            results.into_iter().map(ContentBlock::ToolResult).collect(),
                        );
                        let _ = event_tx
                            .send(EngineToRuntimeEvent::ToolResultsProduced(tool_msg.clone()))
                            .await;
                        ctx.messages.push(tool_msg);
                    }
                    let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                    turns += 1;
                    break;
                }
            };

            if !tool_results.is_empty() {
                had_tool_use = true;
                // 按 assistant 消息中 tool_use 的顺序重排 tool_result
                let result_blocks = Self::order_tool_results_for_message(
                    ctx.messages.last().expect("assistant message just pushed"),
                    tool_results,
                )
                .into_iter()
                .map(ContentBlock::ToolResult)
                .collect();
                let tool_msg = Message::new(Role::User, result_blocks);
                let _ = event_tx
                    .send(EngineToRuntimeEvent::ToolResultsProduced(tool_msg.clone()))
                    .await;
                ctx.messages.push(tool_msg);
            }

            let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
            turns += 1;

            let had_intervention = self
                .drain_pending_user_messages(ctx.messages, &event_tx)
                .await;

            // 如果 LLM 没有请求 tool 调用，结束循环
            if !had_intervention && !matches!(finish_reason, FinishReason::ToolUse) {
                break;
            }
        }

        self.drain_pending_tool_pauses();
        self.clear_pending_user_messages();

        QueryResult {
            turns,
            finish_reason,
            had_tool_use,
        }
    }

    async fn drain_pending_user_messages(
        &self,
        messages: &mut Vec<Message>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> bool {
        let pending = {
            let mut pending = self
                .pending_user_messages
                .lock()
                .expect("pending user messages mutex poisoned");
            pending.drain(..).collect::<Vec<_>>()
        };

        let injected = !pending.is_empty();
        for msg in pending {
            messages.push(msg.clone());
            let _ = event_tx
                .send(EngineToRuntimeEvent::UserMessageProduced(msg))
                .await;
        }
        injected
    }

    fn push_text_delta(blocks: &mut Vec<ContentBlock>, delta: &str) {
        if let Some(ContentBlock::Text(text)) = blocks.last_mut() {
            text.text.push_str(delta);
        } else {
            blocks.push(ContentBlock::Text(TextBlock {
                text: delta.to_string(),
            }));
        }
    }

    fn push_thinking_delta(blocks: &mut Vec<ContentBlock>, delta: &str) {
        if let Some(ContentBlock::Thinking(thinking)) = blocks.last_mut() {
            thinking.thinking.push_str(delta);
        } else {
            blocks.push(ContentBlock::Thinking(ThinkingBlock {
                thinking: delta.to_string(),
            }));
        }
    }

    async fn collect_finished_tool_results(
        tool_tasks: &mut JoinSet<ToolResultBlock>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
        cancelled: &Arc<AtomicBool>,
        cancel_notify: &Notify,
    ) -> ToolCollection {
        let mut tool_results = Vec::new();
        while !tool_tasks.is_empty() {
            if cancelled.load(Ordering::Relaxed) {
                tool_tasks.abort_all();
            }

            let task_result = tokio::select! {
                task_result = tool_tasks.join_next() => task_result,
                _ = cancel_notify.notified() => {
                    if cancelled.load(Ordering::Relaxed) {
                        tool_tasks.abort_all();
                    }
                    continue;
                }
            };

            let Some(task_result) = task_result else {
                break;
            };

            match task_result {
                Ok(tool_result) => {
                    tool_results.push(tool_result);
                }
                Err(join_err) if cancelled.load(Ordering::Relaxed) && join_err.is_cancelled() => {}
                Err(join_err) => {
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::Error(format!(
                            "Tool task panicked: {join_err}"
                        )))
                        .await;
                }
            }
        }
        if cancelled.load(Ordering::Relaxed) {
            ToolCollection::Cancelled(tool_results)
        } else {
            ToolCollection::Completed(tool_results)
        }
    }

    async fn cancel_and_collect_tool_results(
        tool_tasks: &mut JoinSet<ToolResultBlock>,
        assistant_msg: &Message,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> Vec<ToolResultBlock> {
        tool_tasks.abort_all();
        let completed = Self::drain_aborted_tool_tasks(tool_tasks, event_tx).await;
        Self::fill_cancelled_tool_results(assistant_msg, completed, event_tx).await
    }

    async fn fill_cancelled_tool_results(
        assistant_msg: &Message,
        completed: Vec<ToolResultBlock>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> Vec<ToolResultBlock> {
        let mut completed_map: HashMap<String, ToolResultBlock> = completed
            .into_iter()
            .map(|result| (result.tool_use_id.clone(), result))
            .collect();

        let mut ordered = Vec::new();
        for block in &assistant_msg.content {
            if let ContentBlock::ToolUse(tool_use) = block {
                if let Some(result) = completed_map.remove(&tool_use.id) {
                    ordered.push(result);
                } else {
                    let result = Self::cancelled_tool_result(&tool_use.id);
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::ToolResult(result.clone()))
                        .await;
                    ordered.push(result);
                }
            }
        }
        ordered
    }

    async fn drain_aborted_tool_tasks(
        tool_tasks: &mut JoinSet<ToolResultBlock>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> Vec<ToolResultBlock> {
        let mut tool_results = Vec::new();
        while let Some(task_result) = tool_tasks.join_next().await {
            match task_result {
                Ok(tool_result) => tool_results.push(tool_result),
                Err(join_err) if join_err.is_cancelled() => {}
                Err(join_err) => {
                    let _ = event_tx
                        .send(EngineToRuntimeEvent::Error(format!(
                            "Tool task panicked: {join_err}"
                        )))
                        .await;
                }
            }
        }
        tool_results
    }

    fn order_tool_results_for_message(
        assistant_msg: &Message,
        tool_results: Vec<ToolResultBlock>,
    ) -> Vec<ToolResultBlock> {
        let mut tool_result_map: HashMap<String, ToolResultBlock> = tool_results
            .into_iter()
            .map(|r| (r.tool_use_id.clone(), r))
            .collect();

        assistant_msg
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::ToolUse(tu) = block {
                    tool_result_map.remove(&tu.id)
                } else {
                    None
                }
            })
            .collect()
    }

    fn cancelled_tool_result(tool_use_id: &str) -> ToolResultBlock {
        ToolResultBlock {
            tool_use_id: tool_use_id.to_string(),
            is_error: true,
            content: "Execution cancelled".to_string(),
            metadata: None,
        }
    }

    /// 执行单个工具调用。
    ///
    async fn execute_tool(
        tool_registry: &ToolRegistry,
        tool_use: &ToolUseBlock,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
        pending_tool_pauses: PendingToolPauses,
        permission_policy: Arc<dyn PermissionPolicy>,
        cancelled: Arc<AtomicBool>,
    ) -> ToolResultBlock {
        if cancelled.load(Ordering::Relaxed) {
            return ToolResultBlock {
                tool_use_id: tool_use.id.clone(),
                is_error: true,
                content: "Execution cancelled".into(),
                metadata: None,
            };
        }

        let result = if let Some(tool) = tool_registry.get(&tool_use.name) {
            let ctx = ToolExecutionContext {
                tool_use_id: tool_use.id.clone(),
                tool_name: tool_use.name.clone(),
                event_tx: event_tx.clone(),
                pending_tool_pauses,
                permission_policy,
                cancelled,
            };
            tool.execute(tool_use.input.clone(), ctx).await
        } else {
            ToolResult::error(format!("Unknown tool: {}", tool_use.name))
        };

        let block = result.into_block(&tool_use.id);
        let _ = event_tx
            .send(EngineToRuntimeEvent::ToolResult(block.clone()))
            .await;
        block
    }
}

impl Default for QueryEngine {
    fn default() -> Self {
        Self::new()
    }
}
