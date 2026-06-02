use crate::api::{ApiRequest, FinishReason, LlmClient};
use crate::permissions::PermissionEngine;
use crate::runtime::compact::{self, AutoCompactState};
use crate::tools::{
    PendingToolPause, PendingToolPauses, ToolExecutionContext, ToolRegistry, ToolResult,
    ToolRuntimeContext,
};
use crate::types::config::Settings;
use crate::types::events::{ActiveProfile, EngineToRuntimeEvent, ToolPauseResponse};
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

#[derive(Debug, Clone)]
struct ToolRunResult {
    block: ToolResultBlock,
    extra_blocks: Option<Vec<ContentBlock>>,
}

impl ToolRunResult {
    fn new(block: ToolResultBlock, extra_blocks: Option<Vec<ContentBlock>>) -> Self {
        Self {
            block,
            extra_blocks,
        }
    }
}

enum ToolCollection {
    Completed(Vec<ToolRunResult>),
    Cancelled(Vec<ToolRunResult>),
}

struct ToolRunControls {
    settings: Arc<Settings>,
    pending_tool_pauses: PendingToolPauses,
    permission_engine: Arc<PermissionEngine>,
    active_profile: ActiveProfile,
    cancelled: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    runtime_context: Option<Arc<ToolRuntimeContext>>,
    tool_registry: Arc<ToolRegistry>,
}

/// 一次查询的上下文。
///
/// 由 `AgentRuntime` 在每次 `process_run` 时构造传入。
/// `messages` 是引擎本地的工作副本，runtime 通过 `EngineToRuntimeEvent` 获取新消息。
#[derive(Debug)]
pub struct QueryContext<'a> {
    pub messages: &'a mut Vec<Message>,
    pub settings: Arc<Settings>,
    pub llm_client: LlmClient,
    pub tool_registry: Arc<ToolRegistry>,
    pub active_profile: ActiveProfile,
    pub runtime_context: Option<Arc<ToolRuntimeContext>>,
}

/// 查询引擎。
pub struct QueryEngine {
    tool_pause_resolver: ToolPauseResolver,
    permission_engine: Arc<PermissionEngine>,
    cancel_notify: Arc<Notify>,
    pending_user_messages: Mutex<VecDeque<Message>>,
    auto_compact_state: Mutex<AutoCompactState>,
}

#[derive(Clone)]
pub(crate) struct ToolPauseResolver {
    pending_tool_pauses: PendingToolPauses,
}

impl ToolPauseResolver {
    pub(crate) fn new(pending_tool_pauses: PendingToolPauses) -> Self {
        Self {
            pending_tool_pauses,
        }
    }

    pub(crate) fn pending_tool_pauses(&self) -> PendingToolPauses {
        Arc::clone(&self.pending_tool_pauses)
    }

    pub(crate) fn resolve_tool_pause(
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
            (None, _) => Ok(()),
        }
    }

    pub(crate) fn drain_pending_tool_pauses(&self) {
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
}

impl QueryEngine {
    /// 创建新的查询引擎。
    pub fn new(permission_engine: Arc<PermissionEngine>) -> Self {
        Self {
            tool_pause_resolver: ToolPauseResolver::new(Arc::new(Mutex::new(HashMap::new()))),
            permission_engine,
            cancel_notify: Arc::new(Notify::new()),
            pending_user_messages: Mutex::new(VecDeque::new()),
            auto_compact_state: Mutex::new(AutoCompactState::default()),
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
            auto_compact_state: Mutex::new(AutoCompactState::default()),
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
        self.tool_pause_resolver.drain_pending_tool_pauses();
        self.cancel_notify.notify_waiters();
    }

    /// 用户响应工具暂停请求。
    pub fn resolve_tool_pause(
        &self,
        tool_use_id: &str,
        response: ToolPauseResponse,
    ) -> Result<(), String> {
        self.tool_pause_resolver
            .resolve_tool_pause(tool_use_id, response)
    }

    pub(crate) fn tool_pause_resolver(&self) -> ToolPauseResolver {
        self.tool_pause_resolver.clone()
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
        self.tool_pause_resolver.drain_pending_tool_pauses();
        self.clear_pending_user_messages();
        let max_turns = ctx.settings.max_turns.unwrap_or(200);
        let mut turns = 0;
        let mut finish_reason = FinishReason::Stop;
        let mut had_tool_use = false;
        // 在整个 query 生命周期内复用同一个 JoinSet，每轮 drain 后自动清空
        let mut tool_tasks: JoinSet<ToolRunResult> = JoinSet::new();
        // 工具定义在一次 process_run 中不会变化，预先计算一次避免重复 clone
        let tool_definitions = ctx.tool_registry.definitions();

        for _turn in 0..max_turns {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }

            let _ = event_tx.send(EngineToRuntimeEvent::TurnStarted).await;

            let mut compact_state = {
                let mut state = self
                    .auto_compact_state
                    .lock()
                    .expect("auto compact state mutex poisoned");
                std::mem::take(&mut *state)
            };
            let _ = compact::auto_compact_if_needed(
                ctx.messages,
                &ctx.settings,
                &ctx.llm_client,
                &tool_definitions,
                ctx.runtime_context.clone(),
                &event_tx,
                &mut compact_state,
            )
            .await;
            {
                let mut state = self
                    .auto_compact_state
                    .lock()
                    .expect("auto compact state mutex poisoned");
                *state = compact_state;
            }

            let request = ApiRequest {
                messages: ctx.messages,
                model: &ctx.settings.model,
                system_prompt: ctx.settings.system_prompt.as_deref(),
                tools: Some(&tool_definitions),
                max_tokens: None,
                temperature: None,
                thinking_effort: ctx.settings.thinking_effort,
            };

            // TODO: 需要优化api的错误处理, 对于因上下文过长的输入而失败的请求要尝试收缩上下文然后再调研llm摘要
            let mut stream = match ctx.llm_client.invoke(request).await {
                Ok(s) => s,
                Err(e) => {
                    let error = format!("LLM request failed: {e}");
                    finish_reason = FinishReason::Error(error.clone());
                    let _ = event_tx.send(EngineToRuntimeEvent::Error(error)).await;
                    let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                    turns += 1;
                    break;
                }
            };

            let mut stream_completion = None;
            let mut partial_blocks: Vec<ContentBlock> = Vec::new();
            let mut query_cancelled = false;
            let mut stream_error = None;

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
                            let pending_tool_pauses =
                                self.tool_pause_resolver.pending_tool_pauses();
                            let permission_engine = Arc::clone(&self.permission_engine);
                            let cancel_notify = Arc::clone(&self.cancel_notify);
                            let runtime_context = ctx.runtime_context.clone();
                            let controls = ToolRunControls {
                                settings: Arc::clone(&ctx.settings),
                                pending_tool_pauses,
                                permission_engine,
                                active_profile: ctx.active_profile,
                                cancelled,
                                cancel_notify,
                                runtime_context,
                                tool_registry: Arc::clone(&tool_registry),
                            };
                            tool_tasks.spawn(async move {
                                Self::execute_tool(&tool_registry, &tool_use, &tx, controls).await
                            });
                        }
                        crate::api::ApiEvent::Done(completion) => {
                            let _ = event_tx
                                .send(EngineToRuntimeEvent::UsageRecorded(completion.usage))
                                .await;
                            stream_completion = Some(completion);
                        }
                    },
                    Err(stream_err) => {
                        stream_error = Some(format!("Stream error: {stream_err}"));
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

                    self.tool_pause_resolver.drain_pending_tool_pauses();
                    let tool_results = Self::cancel_and_collect_tool_results(
                        &mut tool_tasks,
                        ctx.messages.last().expect("assistant message just pushed"),
                        &event_tx,
                    )
                    .await;

                    if !tool_results.is_empty() {
                        had_tool_use = true;
                        Self::record_tool_results(ctx.messages, tool_results, &event_tx).await;
                    }
                } else {
                    self.tool_pause_resolver.drain_pending_tool_pauses();
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
                    let error =
                        stream_error.unwrap_or_else(|| "Stream ended unexpectedly".to_string());
                    finish_reason = FinishReason::Error(error.clone());
                    had_tool_use |= self
                        .finish_interrupted_turn(
                            ctx.messages,
                            partial_blocks,
                            &mut tool_tasks,
                            &event_tx,
                            error,
                        )
                        .await;
                    turns += 1;
                    break;
                }
            };

            finish_reason = completion.finish_reason.clone();

            let msg = completion.message;
            let assistant_msg_index = if msg.content.is_empty() {
                None
            } else {
                let _ = event_tx
                    .send(EngineToRuntimeEvent::MessageProduced(msg.clone()))
                    .await;
                ctx.messages.push(msg);
                Some(ctx.messages.len() - 1)
            };

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
                    let results = if let Some(index) = assistant_msg_index {
                        Self::fill_cancelled_tool_results(&ctx.messages[index], results, &event_tx)
                            .await
                    } else {
                        results
                    };
                    if Self::completed_turn_missing_assistant_tool_message(
                        assistant_msg_index,
                        &finish_reason,
                        &results,
                    ) {
                        let error = Self::missing_assistant_tool_message_error();
                        finish_reason = FinishReason::Error(error.clone());
                        let _ = event_tx.send(EngineToRuntimeEvent::Error(error)).await;
                        let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                        turns += 1;
                        break;
                    }
                    had_tool_use = !results.is_empty();
                    if !results.is_empty() {
                        Self::record_tool_results(ctx.messages, results, &event_tx).await;
                    }
                    let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                    turns += 1;
                    break;
                }
            };

            if Self::completed_turn_missing_assistant_tool_message(
                assistant_msg_index,
                &finish_reason,
                &tool_results,
            ) {
                let error = Self::missing_assistant_tool_message_error();
                finish_reason = FinishReason::Error(error.clone());
                let _ = event_tx.send(EngineToRuntimeEvent::Error(error)).await;
                let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
                turns += 1;
                break;
            }

            let mut stop_after_permission_denial = false;
            if !tool_results.is_empty() {
                had_tool_use = true;
                // 按 assistant 消息中 tool_use 的顺序重排 tool_result
                let result_blocks = if let Some(index) = assistant_msg_index {
                    Self::order_tool_results_for_message(&ctx.messages[index], tool_results)
                } else {
                    tool_results
                };
                let display_blocks = Self::tool_result_blocks(&result_blocks);
                stop_after_permission_denial = Self::has_main_user_permission_denial_without_note(
                    ctx.runtime_context
                        .as_ref()
                        .map(|runtime| runtime.session_type.as_str()),
                    &display_blocks,
                );
                Self::record_tool_results(ctx.messages, result_blocks, &event_tx).await;
            }

            let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
            turns += 1;

            let had_intervention = self
                .drain_pending_user_messages(ctx.messages, &event_tx)
                .await;

            if Self::should_stop_after_permission_denial(
                stop_after_permission_denial,
                had_intervention,
            ) {
                break;
            }

            // 如果 LLM 没有请求 tool 调用，结束循环
            if !had_intervention && !matches!(finish_reason, FinishReason::ToolUse) {
                break;
            }
        }

        self.tool_pause_resolver.drain_pending_tool_pauses();
        self.clear_pending_user_messages();

        QueryResult {
            turns,
            finish_reason,
            had_tool_use,
        }
    }

    async fn finish_interrupted_turn(
        &self,
        messages: &mut Vec<Message>,
        partial_blocks: Vec<ContentBlock>,
        tool_tasks: &mut JoinSet<ToolRunResult>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
        error: String,
    ) -> bool {
        let _ = event_tx.send(EngineToRuntimeEvent::Error(error)).await;

        let mut had_tool_use = false;
        if !partial_blocks.is_empty() {
            let msg = Message::new(Role::Assistant, partial_blocks);
            let _ = event_tx
                .send(EngineToRuntimeEvent::MessageProduced(msg.clone()))
                .await;
            messages.push(msg);

            self.tool_pause_resolver.drain_pending_tool_pauses();
            let tool_results = Self::cancel_and_collect_tool_results(
                tool_tasks,
                messages.last().expect("assistant message just pushed"),
                event_tx,
            )
            .await;

            if !tool_results.is_empty() {
                had_tool_use = true;
                Self::record_tool_results(messages, tool_results, event_tx).await;
            }
        } else {
            self.tool_pause_resolver.drain_pending_tool_pauses();
            tool_tasks.abort_all();
            while tool_tasks.join_next().await.is_some() {}
        }

        let _ = event_tx.send(EngineToRuntimeEvent::TurnEnded).await;
        had_tool_use
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

    fn has_main_user_permission_denial_without_note(
        runtime_session_type: Option<&str>,
        tool_results: &[ToolResultBlock],
    ) -> bool {
        if runtime_session_type != Some("main") {
            return false;
        }

        tool_results
            .iter()
            .any(Self::is_user_permission_denial_without_note)
    }

    fn should_stop_after_permission_denial(
        has_permission_denial_without_note: bool,
        had_intervention: bool,
    ) -> bool {
        has_permission_denial_without_note && !had_intervention
    }

    fn is_user_permission_denial_without_note(result: &ToolResultBlock) -> bool {
        let Some(metadata) = &result.metadata else {
            return false;
        };

        metadata
            .get("permission_denial_source")
            .and_then(|value| value.as_str())
            == Some("user")
            && metadata
                .get("permission_denied")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            && !metadata
                .get("user_note_present")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
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
        tool_tasks: &mut JoinSet<ToolRunResult>,
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
        tool_tasks: &mut JoinSet<ToolRunResult>,
        assistant_msg: &Message,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> Vec<ToolRunResult> {
        tool_tasks.abort_all();
        let completed = Self::drain_aborted_tool_tasks(tool_tasks, event_tx).await;
        Self::fill_cancelled_tool_results(assistant_msg, completed, event_tx).await
    }

    async fn fill_cancelled_tool_results(
        assistant_msg: &Message,
        completed: Vec<ToolRunResult>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> Vec<ToolRunResult> {
        let mut completed_map: HashMap<String, ToolRunResult> = completed
            .into_iter()
            .map(|result| (result.block.tool_use_id.clone(), result))
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
                    ordered.push(ToolRunResult::new(result, None));
                }
            }
        }
        ordered
    }

    async fn drain_aborted_tool_tasks(
        tool_tasks: &mut JoinSet<ToolRunResult>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) -> Vec<ToolRunResult> {
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
        tool_results: Vec<ToolRunResult>,
    ) -> Vec<ToolRunResult> {
        let mut tool_result_map: HashMap<String, ToolRunResult> = tool_results
            .into_iter()
            .map(|r| (r.block.tool_use_id.clone(), r))
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

    fn tool_result_blocks(tool_results: &[ToolRunResult]) -> Vec<ToolResultBlock> {
        tool_results
            .iter()
            .map(|result| result.block.clone())
            .collect()
    }

    fn completed_turn_missing_assistant_tool_message(
        assistant_msg_index: Option<usize>,
        finish_reason: &FinishReason,
        tool_results: &[ToolRunResult],
    ) -> bool {
        assistant_msg_index.is_none()
            && (matches!(finish_reason, FinishReason::ToolUse) || !tool_results.is_empty())
    }

    fn missing_assistant_tool_message_error() -> String {
        "LLM stream emitted tool activity but Done did not include an assistant tool_use message"
            .to_string()
    }

    async fn record_tool_results(
        messages: &mut Vec<Message>,
        tool_results: Vec<ToolRunResult>,
        event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    ) {
        if tool_results.is_empty() {
            return;
        }

        let (llm_msg, display_msg, has_extra_blocks) = Self::tool_result_messages(tool_results);
        if has_extra_blocks {
            let _ = event_tx
                .send(EngineToRuntimeEvent::LlmHistoryProduced(llm_msg.clone()))
                .await;
            let _ = event_tx
                .send(EngineToRuntimeEvent::ToolResultsDisplayProduced(
                    display_msg,
                ))
                .await;
        } else {
            let _ = event_tx
                .send(EngineToRuntimeEvent::ToolResultsProduced(llm_msg.clone()))
                .await;
        }
        messages.push(llm_msg);
    }

    fn tool_result_messages(tool_results: Vec<ToolRunResult>) -> (Message, Message, bool) {
        let mut display_blocks = Vec::new();
        let mut llm_blocks = Vec::new();
        let mut has_extra_blocks = false;

        for result in tool_results {
            let block = result.block;
            display_blocks.push(ContentBlock::ToolResult(block.clone()));
            llm_blocks.push(ContentBlock::ToolResult(block));

            if let Some(extra_blocks) = result.extra_blocks
                && !extra_blocks.is_empty()
            {
                has_extra_blocks = true;
                llm_blocks.extend(extra_blocks);
            }
        }

        (
            Message::new(Role::User, llm_blocks),
            Message::new(Role::User, display_blocks),
            has_extra_blocks,
        )
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
        controls: ToolRunControls,
    ) -> ToolRunResult {
        if controls.cancelled.load(Ordering::Relaxed) {
            return ToolRunResult::new(
                ToolResultBlock {
                    tool_use_id: tool_use.id.clone(),
                    is_error: true,
                    content: "Execution cancelled".into(),
                    metadata: None,
                },
                None,
            );
        }

        let result = if let Some(tool) = tool_registry.get(&tool_use.name) {
            let runtime_context = controls.runtime_context;
            let ctx = ToolExecutionContext {
                tool_use_id: tool_use.id.clone(),
                pause_id: runtime_context
                    .as_ref()
                    .filter(|runtime| runtime.session_type == "subagent")
                    .map(|runtime| format!("{}:{}", runtime.session_id, tool_use.id))
                    .unwrap_or_else(|| tool_use.id.clone()),
                tool_name: tool_use.name.clone(),
                settings: Some(controls.settings),
                tool_registry: Some(controls.tool_registry),
                event_tx: event_tx.clone(),
                pending_tool_pauses: controls.pending_tool_pauses,
                permission_engine: controls.permission_engine,
                active_profile: controls.active_profile,
                cancelled: controls.cancelled,
                cancel_notify: controls.cancel_notify,
                runtime: runtime_context,
            };
            tool.execute(tool_use.input.clone(), ctx).await
        } else {
            ToolResult::error(format!("Unknown tool: {}", tool_use.name))
        };

        let (block, extra_blocks) = result.into_parts(&tool_use.id);
        let _ = event_tx
            .send(EngineToRuntimeEvent::ToolResult(block.clone()))
            .await;
        ToolRunResult::new(block, extra_blocks)
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
    use crate::types::message::ToolUseBlock;

    fn permission_denied_tool_result(user_note_present: bool) -> ToolResultBlock {
        let mut metadata = serde_json::Map::new();
        metadata.insert("permission_denied".to_string(), serde_json::json!(true));
        metadata.insert(
            "user_note_present".to_string(),
            serde_json::json!(user_note_present),
        );
        metadata.insert(
            "permission_denial_source".to_string(),
            serde_json::json!("user"),
        );

        ToolResultBlock {
            tool_use_id: "toolu_denied".to_string(),
            is_error: true,
            content: "Permission denied for tool: bash".to_string(),
            metadata: Some(metadata),
        }
    }

    fn text_from_message(msg: &Message) -> String {
        msg.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn resolving_unknown_tool_pause_is_idempotent() {
        let resolver = ToolPauseResolver::new(Arc::new(Mutex::new(HashMap::new())));

        let result = resolver.resolve_tool_pause(
            "toolu_done",
            ToolPauseResponse::Permission {
                approved: false,
                note: None,
            },
        );

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn main_user_permission_denial_without_note_stops_next_query() {
        let tool_results = vec![permission_denied_tool_result(false)];

        assert!(QueryEngine::has_main_user_permission_denial_without_note(
            Some("main"),
            &tool_results
        ));
        assert!(QueryEngine::should_stop_after_permission_denial(
            true, false
        ));
    }

    #[test]
    fn main_user_permission_denial_with_note_continues_next_query() {
        let tool_results = vec![permission_denied_tool_result(true)];

        assert!(!QueryEngine::has_main_user_permission_denial_without_note(
            Some("main"),
            &tool_results
        ));
    }

    #[test]
    fn main_user_permission_denial_with_intervention_continues_next_query() {
        assert!(!QueryEngine::should_stop_after_permission_denial(
            true, true
        ));
    }

    #[test]
    fn subagent_user_permission_denial_without_note_continues_next_query() {
        let tool_results = vec![permission_denied_tool_result(false)];

        assert!(!QueryEngine::has_main_user_permission_denial_without_note(
            Some("subagent"),
            &tool_results
        ));
    }

    #[test]
    fn configured_permission_denial_continues_next_query() {
        let tool_results = vec![ToolResultBlock {
            tool_use_id: "toolu_denied".to_string(),
            is_error: true,
            content: "denied by config".to_string(),
            metadata: None,
        }];

        assert!(!QueryEngine::has_main_user_permission_denial_without_note(
            Some("main"),
            &tool_results
        ));
    }

    #[test]
    fn tool_result_messages_keep_extra_blocks_only_in_llm_message() {
        let result = ToolRunResult::new(
            ToolResultBlock {
                tool_use_id: "toolu_image".to_string(),
                is_error: false,
                content: "Loaded image".to_string(),
                metadata: None,
            },
            Some(vec![ContentBlock::from_base64_image(
                "image/png".to_string(),
                "abc123".to_string(),
            )]),
        );

        let (llm_msg, display_msg, has_extra_blocks) =
            QueryEngine::tool_result_messages(vec![result]);

        assert!(has_extra_blocks);
        assert_eq!(llm_msg.content.len(), 2);
        assert!(matches!(llm_msg.content[0], ContentBlock::ToolResult(_)));
        assert!(matches!(llm_msg.content[1], ContentBlock::Image(_)));
        assert_eq!(display_msg.content.len(), 1);
        assert!(matches!(
            display_msg.content[0],
            ContentBlock::ToolResult(_)
        ));
    }

    #[tokio::test]
    async fn record_tool_results_ignores_empty_batches() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut messages = Vec::new();

        QueryEngine::record_tool_results(&mut messages, Vec::new(), &tx).await;

        assert!(messages.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn completed_turn_requires_assistant_message_for_tool_activity() {
        let result = ToolRunResult::new(
            ToolResultBlock {
                tool_use_id: "toolu_image".to_string(),
                is_error: false,
                content: "Loaded image".to_string(),
                metadata: None,
            },
            None,
        );

        assert!(QueryEngine::completed_turn_missing_assistant_tool_message(
            None,
            &FinishReason::ToolUse,
            &[]
        ));
        assert!(QueryEngine::completed_turn_missing_assistant_tool_message(
            None,
            &FinishReason::Stop,
            &[result]
        ));
        assert!(!QueryEngine::completed_turn_missing_assistant_tool_message(
            Some(0),
            &FinishReason::ToolUse,
            &[]
        ));
    }

    #[tokio::test]
    async fn interrupted_turn_preserves_partial_text() {
        let engine = QueryEngine::default();
        let (tx, mut rx) = mpsc::channel(16);
        let mut messages = Vec::new();
        let mut tool_tasks = JoinSet::new();

        let had_tool_use = engine
            .finish_interrupted_turn(
                &mut messages,
                vec![ContentBlock::from_text("partial answer".to_string())],
                &mut tool_tasks,
                &tx,
                "Stream ended unexpectedly".to_string(),
            )
            .await;

        assert!(!had_tool_use);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(text_from_message(&messages[0]), "partial answer");

        let mut saw_error = false;
        let mut saw_message = false;
        let mut saw_turn_end = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                EngineToRuntimeEvent::Error(error) => {
                    saw_error = error == "Stream ended unexpectedly";
                }
                EngineToRuntimeEvent::MessageProduced(msg) => {
                    saw_message = text_from_message(&msg) == "partial answer";
                }
                EngineToRuntimeEvent::TurnEnded => {
                    saw_turn_end = true;
                }
                _ => {}
            }
        }

        assert!(saw_error);
        assert!(saw_message);
        assert!(saw_turn_end);
    }

    #[tokio::test]
    async fn interrupted_turn_without_partial_text_does_not_create_message() {
        let engine = QueryEngine::default();
        let (tx, mut rx) = mpsc::channel(16);
        let mut messages = Vec::new();
        let mut tool_tasks = JoinSet::new();

        let had_tool_use = engine
            .finish_interrupted_turn(
                &mut messages,
                Vec::new(),
                &mut tool_tasks,
                &tx,
                "Stream ended unexpectedly".to_string(),
            )
            .await;

        assert!(!had_tool_use);
        assert!(messages.is_empty());

        let mut saw_message = false;
        let mut saw_turn_end = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                EngineToRuntimeEvent::MessageProduced(_) => saw_message = true,
                EngineToRuntimeEvent::TurnEnded => saw_turn_end = true,
                _ => {}
            }
        }

        assert!(!saw_message);
        assert!(saw_turn_end);
    }

    #[tokio::test]
    async fn interrupted_turn_fills_cancelled_tool_results() {
        let engine = QueryEngine::default();
        let (tx, mut rx) = mpsc::channel(16);
        let mut messages = Vec::new();
        let mut tool_tasks = JoinSet::new();
        let tool_use = ToolUseBlock {
            id: "toolu_1".to_string(),
            name: "read".to_string(),
            input: HashMap::new(),
        };

        let had_tool_use = engine
            .finish_interrupted_turn(
                &mut messages,
                vec![ContentBlock::ToolUse(tool_use)],
                &mut tool_tasks,
                &tx,
                "Stream ended unexpectedly".to_string(),
            )
            .await;

        assert!(had_tool_use);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[1].role, Role::User);

        let tool_result = messages[1]
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolResult(result) => Some(result),
                _ => None,
            })
            .expect("cancelled tool result should be appended");
        assert_eq!(tool_result.tool_use_id, "toolu_1");
        assert!(tool_result.is_error);
        assert_eq!(tool_result.content, "Execution cancelled");

        let mut saw_tool_result_event = false;
        let mut saw_tool_results_message = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                EngineToRuntimeEvent::ToolResult(result) => {
                    saw_tool_result_event = result.tool_use_id == "toolu_1";
                }
                EngineToRuntimeEvent::ToolResultsProduced(msg) => {
                    saw_tool_results_message = msg.role == Role::User;
                }
                _ => {}
            }
        }

        assert!(saw_tool_result_event);
        assert!(saw_tool_results_message);
    }
}
