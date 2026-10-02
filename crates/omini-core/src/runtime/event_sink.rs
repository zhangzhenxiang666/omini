//! 共享执行事件接收端：主 Run 与子 Agent 任务共用的引擎事件处理。
//!
//! 引擎（QueryEngine）产生 `EngineToRuntimeEvent`，这里统一完成两件事：
//! - 经宿主执行持久化（消息、步骤、工具调用、用量、Run 状态）；
//! - 投影为输出领域事件（主 Run 直接发 `RuntimeToServerEvent`，子任务包进
//!   `AgentTaskEvent` 信封）。
//!
//! 关键持久化成功后才发布对应提交事件；失败返回引擎提交边界，阻止后续执行，不报告成功。

use super::active_run;
use super::manual_compact::persist_compact_summary_event;
use super::plan;
use super::service::AgentRuntime;
use super::usage::{record_total_usage_and_notify, record_usage_snapshot};
use super::*;
use crate::agent::AgentTaskSupervisor;
use crate::execution::handle::OutputHandle;
use crate::execution::host::AgentHost;
use omini_domain::agent_run::{
    AgentRunStatus, AgentStepSnapshot, AgentStepStatus, ToolUseExecutionSnapshot, ToolUseStatus,
};
use omini_runtime_contract::thread_domain::{
    AgentTaskEvent, AgentTaskEventEnvelope, AgentTaskInfo, ThreadUsageSnapshot, ToolPauseKind,
    ToolPauseResponse,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use tracing::Instrument;

/// 透传事件的投影中立表示，之后按运行身份编码为主 Run 事件或任务信封。
enum StreamEvent {
    TurnStarted,
    TurnEnded,
    ThinkingDelta { delta: String },
    TextDelta { delta: String },
    ToolUse { tool_use: ToolUseBlock },
    ToolResult { tool_result: ToolResultBlock },
}

/// 运行账本在两种投影间的身份差异。
enum Projection {
    /// 主 Run：直接输出 `RuntimeToServerEvent`，账本记在 run_id 名下。
    /// 字段装箱避免与子任务变体的尺寸差异撑大整个枚举。
    Main(Box<MainProjection>),
    /// 子 Agent 任务：事件包进 `AgentTaskEvent` 信封，账本记在 task_id（即子 Run）名下。
    Child(Box<ChildProjection>),
}

/// 主 Run 投影的执行资源与账本身份。
struct MainProjection {
    thread_id: String,
    model_ref: String,
    context_window: Option<u32>,
    usage_state: Arc<Mutex<ThreadUsageSnapshot>>,
    task_supervisor: Arc<AgentTaskSupervisor>,
}

/// 子 Agent 任务投影的身份与账本身份。
struct ChildProjection {
    info: AgentTaskInfo,
    model_ref: String,
    owner_usage: Arc<Mutex<ThreadUsageSnapshot>>,
}

impl AgentRuntime {
    /// 为主 Run 启动共享事件接收端任务。
    pub(crate) async fn spawn_run_sink(
        &self,
        engine_rx: mpsc::Receiver<EngineToRuntimeEvent>,
        active_profile: ActiveProfile,
        active_profile_handle: Arc<RwLock<ActiveProfile>>,
        tool_pause_resolver: ToolPauseResolver,
        run_id: Option<String>,
    ) -> tokio::task::JoinHandle<Option<String>> {
        let model = self.settings.active_model();
        let sink = EngineEventSink {
            host: Arc::clone(&self.host),
            output: self.output.clone(),
            active_profile_handle,
            tool_pause_resolver,
            run_key: run_id,
            next_step_no: 0,
            active_step_id: None,
            tool_uses: HashMap::new(),
            warnings: Vec::new(),
            persistence_failure: Mutex::new(None),
            proposed_plan_forwarder: Some(plan::ProposedPlanForwarder::new(active_profile)),
            projection: Projection::Main(Box::new(MainProjection {
                thread_id: self.thread_id.clone(),
                model_ref: format!("{}/{}", model.provider_id, model.model_id),
                context_window: active_run::current_context_window(&self.settings),
                usage_state: Arc::clone(&self.thread_usage),
                task_supervisor: Arc::clone(&self.task_supervisor),
            })),
        };
        let span = tracing::debug_span!("event_sink", thread_id = %self.thread_id);
        tokio::spawn(
            async move {
                let mut sink = sink;
                sink.run(engine_rx).await;
                sink.failure()
            }
            .instrument(span),
        )
    }
}

/// 子执行接收端所需的身份、宿主与共享资源。
pub(crate) struct ChildSinkConfig {
    pub host: Arc<dyn AgentHost>,
    pub output: OutputHandle,
    pub active_profile_handle: Arc<RwLock<ActiveProfile>>,
    pub pending_tool_pauses: crate::tools::PendingToolPauses,
    pub info: AgentTaskInfo,
    pub model_ref: String,
    pub owner_usage: Arc<Mutex<ThreadUsageSnapshot>>,
}

/// 为子 Agent 启动共享事件接收端，按子任务身份投影并汇总失败原因。
pub(crate) fn spawn_child_sink(
    config: ChildSinkConfig,
    engine_rx: mpsc::Receiver<EngineToRuntimeEvent>,
) -> tokio::task::JoinHandle<Vec<String>> {
    let ChildSinkConfig {
        host,
        output,
        active_profile_handle,
        pending_tool_pauses,
        info,
        model_ref,
        owner_usage,
    } = config;
    let sink = EngineEventSink {
        host,
        output,
        active_profile_handle,
        tool_pause_resolver: ToolPauseResolver::new(pending_tool_pauses),
        run_key: Some(info.task_id.clone()),
        next_step_no: 0,
        active_step_id: None,
        tool_uses: HashMap::new(),
        warnings: Vec::new(),
        persistence_failure: Mutex::new(None),
        proposed_plan_forwarder: None,
        projection: Projection::Child(Box::new(ChildProjection {
            info,
            model_ref,
            owner_usage,
        })),
    };
    let span = tracing::debug_span!(
        "event_sink",
        thread_id = %sink_child_thread_id(&sink.projection),
        task_kind = "child_event_sink"
    );
    tokio::spawn(
        async move {
            let mut sink = sink;
            sink.run(engine_rx).await;
            std::mem::take(&mut sink.warnings)
        }
        .instrument(span),
    )
}

fn sink_child_thread_id(projection: &Projection) -> String {
    match projection {
        Projection::Main(main) => main.thread_id.clone(),
        Projection::Child(child) => child.info.thread_id.clone(),
    }
}

/// 共享事件接收端。
struct EngineEventSink {
    host: Arc<dyn AgentHost>,
    output: OutputHandle,
    active_profile_handle: Arc<RwLock<ActiveProfile>>,
    tool_pause_resolver: ToolPauseResolver,
    /// 账本归属的 Run 身份：主 Run 的 run_id 或子任务的 task_id。
    run_key: Option<String>,
    next_step_no: u32,
    active_step_id: Option<String>,
    tool_uses: HashMap<String, ToolUseExecutionSnapshot>,
    warnings: Vec<String>,
    persistence_failure: Mutex<Option<String>>,
    /// 主 Run 专有：文本增量中的 proposed_plan 拆分转发。
    proposed_plan_forwarder: Option<plan::ProposedPlanForwarder>,
    projection: Projection,
}

impl EngineEventSink {
    async fn run(&mut self, mut rx: mpsc::Receiver<EngineToRuntimeEvent>) {
        while let Some(event) = rx.recv().await {
            self.handle(event).await;
        }
    }

    fn failure(&self) -> Option<String> {
        self.persistence_failure
            .lock()
            .expect("sink failure lock poisoned")
            .clone()
    }

    fn fail_persistence(&self, error: impl std::fmt::Display) {
        let mut failure = self
            .persistence_failure
            .lock()
            .expect("sink failure lock poisoned");
        if failure.is_none() {
            *failure = Some(error.to_string());
        }
    }

    async fn handle(&mut self, event: EngineToRuntimeEvent) {
        let active = *self
            .active_profile_handle
            .read()
            .expect("active profile lock poisoned");
        match event {
            EngineToRuntimeEvent::CommitBarrier { ack } => {
                let _ = ack.send(self.failure().map_or(Ok(()), Err));
            }
            // ===== 需要持久化的事件 =====
            EngineToRuntimeEvent::UserMessageProduced(message) => match &self.projection {
                // 展示行与 echo 由宿主在接收输入时处理，运行时只追加 LLM 上下文行。
                Projection::Main(main) => {
                    self.append_llm(&main.thread_id, &message).await;
                }
                Projection::Child(..) => {
                    self.child_persist_message(&message, true, true).await;
                }
            },
            EngineToRuntimeEvent::TaskMessageProduced {
                message,
                source,
                ack,
            } => match &self.projection {
                Projection::Main(..) => {
                    let _ = ack.send(Err(
                        "agent-to-agent message reached the main runtime".to_string()
                    ));
                }
                Projection::Child(child) => {
                    let key = source.delivery_key(&child.info.task_id);
                    let result = self
                        .host
                        .inject_task_message(&key, &child.info.thread_id, &message)
                        .await
                        .map_err(|error| error.to_string());
                    if let Err(error) = &result {
                        self.warnings.push(error.clone());
                    }
                    let _ = ack.send(result);
                }
            },
            EngineToRuntimeEvent::TaskNotificationsProduced { notification, ack } => {
                match &self.projection {
                    // 完成通知先持久化，成功后才标记送达并允许后续运行继续。
                    Projection::Main(main) => {
                        let result = self
                            .host
                            .insert_task_notification(&main.thread_id, &notification)
                            .await
                            .map_err(|error| error.to_string());
                        match &result {
                            Ok(_) => {
                                let task_ids = notification
                                    .tasks
                                    .iter()
                                    .map(|completion| completion.task_id.clone())
                                    .collect::<Vec<_>>();
                                // 成功确认即提交的全部任务已交付：新内容本次写入，
                                // 其余任务此前已写入。把全部提交身份从在途集合
                                // 退役，否则部分重叠/重复请求会在待交付集合中
                                // 留下永不清理的身份，实例无法回到可回收空闲态。
                                // ack 回传与注入仍只携带 fresh 部分。
                                main.task_supervisor.mark_notifications_delivered(&task_ids);
                            }
                            Err(_) => {
                                // 失败保留在途身份，由引擎重试。
                            }
                        }
                        // 回带实际新交付的任务 ID：引擎据此只注入新内容，
                        // 全部已交付时不注入，避免模型上下文出现重复通知。
                        let _ = ack.send(result.map(|fresh| {
                            fresh.map(|notification| {
                                notification
                                    .tasks
                                    .into_iter()
                                    .map(|completion| completion.task_id)
                                    .collect::<Vec<String>>()
                            })
                        }));
                    }
                    Projection::Child(..) => {
                        let _ = ack.send(Err(
                            "background task notifications are only supported by the main engine"
                                .to_string(),
                        ));
                    }
                }
            }
            EngineToRuntimeEvent::LlmHistoryProduced(msg) => match &self.projection {
                Projection::Main(main) => {
                    self.append_llm(&main.thread_id, &msg).await;
                }
                Projection::Child(..) => {
                    self.child_persist_message(&msg, true, false).await;
                }
            },
            EngineToRuntimeEvent::ReplaceLlmContext {
                thread_id,
                expected_version,
                messages,
                ack,
            } => {
                let result = match self
                    .host
                    .replace_llm_context(&thread_id, expected_version, messages)
                    .await
                {
                    Ok(version) => Ok(version),
                    Err(error) => Err(error.to_string()),
                };
                let _ = ack.send(result);
            }
            EngineToRuntimeEvent::ToolResultsDisplayProduced(msg) => match &self.projection {
                Projection::Main(main) => {
                    self.append_ui(&main.thread_id, &msg, active, &main.model_ref)
                        .await;
                }
                Projection::Child(..) => {
                    self.child_persist_message(&msg, false, true).await;
                }
            },
            EngineToRuntimeEvent::MessageProduced(msg)
            | EngineToRuntimeEvent::ToolResultsProduced(msg) => match &self.projection {
                Projection::Main(main) => {
                    self.append_llm(&main.thread_id, &msg).await;
                    self.append_ui(&main.thread_id, &msg, active, &main.model_ref)
                        .await;
                }
                Projection::Child(..) => {
                    self.child_persist_message(&msg, true, true).await;
                }
            },
            // ===== 透传事件 =====
            EngineToRuntimeEvent::TurnStarted => {
                tracing::debug!("forwarding turn started event");
                self.begin_step().await;
                self.emit(StreamEvent::TurnStarted).await;
            }
            EngineToRuntimeEvent::TurnEnded => {
                tracing::debug!("forwarding turn ended event");
                self.end_step().await;
                if let Some(forwarder) = &mut self.proposed_plan_forwarder {
                    forwarder.flush(&self.output).await;
                }
                self.emit(StreamEvent::TurnEnded).await;
            }
            EngineToRuntimeEvent::ThinkingDelta(delta) => {
                self.emit(StreamEvent::ThinkingDelta { delta }).await;
            }
            EngineToRuntimeEvent::TextDelta(delta) => match &mut self.proposed_plan_forwarder {
                Some(forwarder) => {
                    forwarder.forward_text_delta(&self.output, delta).await;
                }
                None => {
                    self.emit(StreamEvent::TextDelta { delta }).await;
                }
            },
            EngineToRuntimeEvent::ToolUse(tool_use) => {
                tracing::debug!(
                    tool_use_id = %tool_use.id,
                    tool_name = %tool_use.name,
                    "forwarding tool use event"
                );
                self.record_tool_use_running(&tool_use).await;
                self.emit(StreamEvent::ToolUse { tool_use }).await;
            }
            EngineToRuntimeEvent::ToolResult(tool_result) => {
                tracing::debug!(
                    tool_use_id = %tool_result.tool_use_id,
                    is_error = tool_result.is_error,
                    "forwarding tool result event"
                );
                self.record_tool_use_settled(&tool_result).await;
                self.emit(StreamEvent::ToolResult { tool_result }).await;
            }
            EngineToRuntimeEvent::ToolPauseRequested(request) => {
                self.handle_tool_pause(*request).await;
            }
            EngineToRuntimeEvent::UsageRecorded(usage) => {
                tracing::debug!(
                    prompt_tokens = usage.prompt_tokens,
                    completion_tokens = usage.completion_tokens,
                    cached_tokens = usage.cached_tokens,
                    "recording usage"
                );
                self.record_usage(usage).await;
            }
            EngineToRuntimeEvent::CompactShrinkStarted(_)
            | EngineToRuntimeEvent::CompactShrinkFinished(_)
            | EngineToRuntimeEvent::CompactShrinkFailed(_) => {
                tracing::debug!("compact shrink event received");
                // TODO(compact): 收缩操作暂不通知 UI，后续再决定是否记录内部状态。
            }
            EngineToRuntimeEvent::CompactSummaryStarted(event) => {
                tracing::debug!(
                    trigger = %event.trigger,
                    compact_thread_id = ?event.thread_id,
                    agent_label = ?event.agent_label,
                    "compact summary started"
                );
                if let Projection::Main(..) = self.projection {
                    let _ = self
                        .output
                        .send_event(RuntimeToServerEvent::CompactSummaryStarted(event))
                        .await;
                }
            }
            EngineToRuntimeEvent::CompactSummaryDelta(event) => {
                if let Projection::Main(..) = self.projection {
                    let _ = self
                        .output
                        .send_event(RuntimeToServerEvent::CompactSummaryDelta(event))
                        .await;
                }
            }
            EngineToRuntimeEvent::CompactSummaryFinished(event) => {
                tracing::debug!(
                    trigger = %event.trigger,
                    compact_thread_id = ?event.thread_id,
                    agent_label = ?event.agent_label,
                    summary_chars = event.summary.chars().count(),
                    after_tokens = event.after_tokens,
                    "compact summary finished"
                );
                if let Projection::Main(main) = &self.projection {
                    persist_compact_summary_event(
                        &main.thread_id,
                        &event,
                        &main.model_ref,
                        self.host.as_ref(),
                    )
                    .await;
                    let _ = self
                        .output
                        .send_event(RuntimeToServerEvent::CompactSummaryFinished(event))
                        .await;
                }
            }
            EngineToRuntimeEvent::CompactSummaryFailed(event) => {
                tracing::warn!(
                    trigger = %event.trigger,
                    compact_thread_id = ?event.thread_id,
                    agent_label = ?event.agent_label,
                    message = %event.message,
                    "compact summary failed"
                );
                if let Projection::Main(..) = self.projection {
                    let _ = self
                        .output
                        .send_event(RuntimeToServerEvent::CompactSummaryFailed(event))
                        .await;
                }
            }
            EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage) => {
                tracing::debug!(
                    prompt_tokens = usage.prompt_tokens,
                    completion_tokens = usage.completion_tokens,
                    cached_tokens = usage.cached_tokens,
                    "recording compact summary usage"
                );
                self.record_compact_usage(usage).await;
            }
            EngineToRuntimeEvent::Error(error) => {
                tracing::warn!(error = %error, "runtime engine error");
                match &self.projection {
                    Projection::Main(..) => {
                        let _ = self
                            .output
                            .send_event(RuntimeToServerEvent::error(error))
                            .await;
                    }
                    Projection::Child(..) => self.warnings.push(error),
                }
            }
            EngineToRuntimeEvent::Warning(warning) => {
                tracing::warn!(warning = %warning, "runtime engine warning");
                match &self.projection {
                    Projection::Main(..) => {
                        let _ = self
                            .output
                            .send_event(RuntimeToServerEvent::warning(warning))
                            .await;
                    }
                    Projection::Child(..) => self.warnings.push(warning),
                }
            }
        }
    }

    /// 主线程：追加 LLM 上下文行。
    async fn append_llm(&self, thread_id: &str, message: &Message) {
        if let Err(error) = self.host.append_llm_message(thread_id, message).await {
            self.fail_persistence(&error);
            tracing::error!(thread_id, error = %error, "failed to append llm message");
        }
    }

    /// 主线程：追加 UI 展示行；UI 块按 active_profile 剥离 plan 块。
    async fn append_ui(
        &self,
        thread_id: &str,
        message: &Message,
        active_profile: ActiveProfile,
        model_ref: &str,
    ) {
        let ui_message = history::ui_display_message(message, active_profile);
        let ui_model_ref = history::model_ref_for_role(message.role, model_ref);
        if let Err(error) = self
            .host
            .append_ui_message(thread_id, &ui_message, ui_model_ref.as_deref())
            .await
        {
            self.fail_persistence(&error);
            tracing::error!(thread_id, error = %error, "failed to append ui message");
        }
    }

    /// 子任务：持久化消息，成功且需要展示时发布 `MessageCommitted`。
    ///
    /// 先取子任务身份的 owned 副本，避免持有 `self.projection` 的借用
    /// 同时向 `warnings` 推送。
    async fn child_persist_message(
        &mut self,
        message: &Message,
        persist_llm_history: bool,
        display_in_ui: bool,
    ) {
        let (thread_id, task_id, parent_task_id, owner_thread_id, model_ref) =
            match &self.projection {
                Projection::Child(child) => (
                    child.info.thread_id.clone(),
                    child.info.task_id.clone(),
                    child.info.parent_task_id.clone(),
                    child.info.owner_thread_id.clone(),
                    child.model_ref.clone(),
                ),
                Projection::Main(..) => return,
            };
        let model_ref = history::model_ref_for_role(message.role, &model_ref);
        let result = self
            .host
            .persist_agent_message(
                &thread_id,
                message,
                model_ref.as_deref(),
                persist_llm_history,
                display_in_ui,
            )
            .await;
        match result {
            Ok(()) => {
                if display_in_ui {
                    let _ = self
                        .output
                        .send_event(RuntimeToServerEvent::AgentTaskEvent(
                            AgentTaskEventEnvelope {
                                task_id,
                                thread_id,
                                parent_task_id,
                                owner_thread_id,
                                truncated: false,
                                payload: AgentTaskEvent::MessageCommitted {
                                    message: message.clone(),
                                    persist_llm_history,
                                },
                            },
                        ))
                        .await;
                }
            }
            Err(error) => {
                self.fail_persistence(&error);
                self.warnings.push(error.to_string());
            }
        }
    }

    async fn handle_tool_pause(
        &mut self,
        request: omini_runtime_contract::thread_domain::ToolPauseRequest,
    ) {
        tracing::debug!(
            tool_use_id = %request.tool_use_id,
            tool_name = %request.tool_name,
            source_thread_id = ?request.source_thread_id,
            source_agent_label = ?request.source_agent_label,
            pause_kind = ?request.kind,
            "tool pause requested"
        );
        let is_permission = matches!(request.kind, ToolPauseKind::Permission(_));
        if is_permission {
            self.record_pause_waiting_approval(&request.tool_use_id)
                .await;
        }
        let active_profile = *self
            .active_profile_handle
            .read()
            .expect("active profile lock poisoned");
        if active_profile == ActiveProfile::Auto && is_permission {
            tracing::debug!(
                tool_use_id = %request.tool_use_id,
                tool_name = %request.tool_name,
                "auto approving permission pause"
            );
            if let Err(error) = self.tool_pause_resolver.resolve_tool_pause(
                &request.tool_use_id,
                ToolPauseResponse::Permission {
                    approved: true,
                    note: None,
                },
            ) {
                tracing::warn!(
                    tool_use_id = %request.tool_use_id,
                    error = %error,
                    "failed to auto approve permission pause"
                );
                let _ = self
                    .output
                    .send_event(RuntimeToServerEvent::error(error.to_string()))
                    .await;
            }
            return;
        }
        let _ = self
            .output
            .send_event(RuntimeToServerEvent::ToolPauseRequested(request))
            .await;
    }

    /// 权限暂停时把工具调用与 Run 状态记为等待审批。
    async fn record_pause_waiting_approval(&self, tool_use_id: &str) {
        let Some(run_key) = self.run_key.clone() else {
            return;
        };
        if let Some(mut record) = self.tool_uses.get(tool_use_id).cloned() {
            record.updated_at = Timestamp::now();
            record.status = ToolUseStatus::WaitingApproval;
            if let Err(error) = self
                .host
                .upsert_tool_use_execution(&record, ToolUseStatus::WaitingApproval)
                .await
            {
                tracing::warn!(error = %error, "failed to record waiting approval tool use");
            }
        }
        if let Err(error) = self
            .host
            .update_agent_run(&run_key, AgentRunStatus::WaitingApproval, None, None, 0)
            .await
        {
            tracing::warn!(error = %error, "failed to record waiting approval run");
        }
    }

    async fn begin_step(&mut self) {
        let Some(run_key) = self.run_key.clone() else {
            return;
        };
        self.next_step_no += 1;
        let step_id = uuid::Uuid::new_v4().to_string();
        let step = AgentStepSnapshot {
            id: step_id.clone(),
            run_id: run_key,
            step_no: self.next_step_no,
            status: AgentStepStatus::Running,
            started_at: Timestamp::now(),
            finished_at: None,
            input_tokens: 0,
            output_tokens: 0,
        };
        if let Err(error) = self.host.upsert_agent_step(&step).await {
            self.fail_persistence(&error);
            tracing::warn!(error = %error, "failed to upsert agent step");
        }
        self.active_step_id = Some(step_id);
    }

    async fn end_step(&mut self) {
        let Some(step_id) = self.active_step_id.take() else {
            return;
        };
        if let Err(error) = self
            .host
            .update_agent_step(
                &step_id,
                AgentStepStatus::Completed,
                Some(Timestamp::now()),
                0,
                0,
            )
            .await
        {
            self.fail_persistence(&error);
            tracing::warn!(error = %error, "failed to update agent step");
        }
    }

    async fn record_tool_use_running(&mut self, tool_use: &ToolUseBlock) {
        let Some(step_id) = self.active_step_id.clone() else {
            return;
        };
        let record = ToolUseExecutionSnapshot {
            id: tool_use.id.clone(),
            step_id,
            name: tool_use.name.clone(),
            input: serde_json::to_value(&tool_use.input).unwrap_or(serde_json::Value::Null),
            status: ToolUseStatus::Running,
            updated_at: Timestamp::now(),
        };
        if let Err(error) = self
            .host
            .upsert_tool_use_execution(&record, ToolUseStatus::Running)
            .await
        {
            self.fail_persistence(&error);
            tracing::warn!(error = %error, "failed to record tool use");
        }
        self.tool_uses.insert(tool_use.id.clone(), record);
    }

    async fn record_tool_use_settled(&mut self, tool_result: &ToolResultBlock) {
        let Some(mut record) = self.tool_uses.get(&tool_result.tool_use_id).cloned() else {
            return;
        };
        record.updated_at = Timestamp::now();
        let status = if tool_result.is_error {
            ToolUseStatus::Failed
        } else {
            ToolUseStatus::Completed
        };
        record.status = status;
        if let Err(error) = self.host.upsert_tool_use_execution(&record, status).await {
            self.fail_persistence(&error);
            tracing::warn!(error = %error, "failed to settle tool use");
        }
    }

    async fn record_usage(&mut self, usage: omini_domain::usage::Usage) {
        let run_tokens = usage.total_tokens() as i64;
        match &self.projection {
            Projection::Main(main) => {
                if let Err(error) = self.host.record_thread_usage(&main.thread_id, usage).await {
                    tracing::warn!(error = %error, "failed to record thread usage");
                }
                if let Some(run_key) = &self.run_key {
                    let _ = self
                        .host
                        .update_agent_run(run_key, AgentRunStatus::Running, None, None, run_tokens)
                        .await;
                    if let Some(step_id) = &self.active_step_id {
                        let _ = self
                            .host
                            .update_agent_step(
                                step_id,
                                AgentStepStatus::Running,
                                None,
                                usage.prompt_tokens as i64,
                                usage.completion_tokens as i64,
                            )
                            .await;
                    }
                }
                let snapshot = record_usage_snapshot(&main.usage_state, usage, main.context_window);
                let _ = self
                    .output
                    .send_event(RuntimeToServerEvent::UsageChanged(snapshot))
                    .await;
            }
            Projection::Child(child) => {
                let info = &child.info;
                if let Err(error) = self.host.record_thread_usage(&info.thread_id, usage).await {
                    tracing::warn!(error = %error, "failed to record child thread usage");
                }
                let _ = self
                    .host
                    .update_agent_run(
                        &info.task_id,
                        AgentRunStatus::Running,
                        None,
                        None,
                        run_tokens,
                    )
                    .await;
                if let Some(step_id) = &self.active_step_id {
                    let _ = self
                        .host
                        .update_agent_step(
                            step_id,
                            AgentStepStatus::Running,
                            None,
                            usage.prompt_tokens as i64,
                            usage.completion_tokens as i64,
                        )
                        .await;
                }
                self.record_owner_usage(&info.clone(), usage).await;
            }
        }
    }

    async fn record_compact_usage(&mut self, usage: omini_domain::usage::Usage) {
        match &self.projection {
            Projection::Main(main) => {
                record_total_usage_and_notify(
                    &main.thread_id,
                    usage,
                    &self.output,
                    self.host.as_ref(),
                    &main.usage_state,
                )
                .await;
            }
            Projection::Child(child) => {
                let _ = self
                    .host
                    .record_thread_total_usage(&child.info.thread_id, usage)
                    .await;
                self.record_owner_usage(&child.info.clone(), usage).await;
            }
        }
    }

    /// 子任务用量归属 owner 总量并广播总量变化。
    async fn record_owner_usage(&self, info: &AgentTaskInfo, usage: omini_domain::usage::Usage) {
        let _ = self
            .host
            .record_owner_agent_usage(&info.owner_thread_id, usage)
            .await;
        let Projection::Child(child) = &self.projection else {
            return;
        };
        let owner_usage = &child.owner_usage;
        let (total_tokens, total_cached_tokens) = {
            let mut snapshot = owner_usage.lock().expect("owner usage lock poisoned");
            snapshot.total_tokens = snapshot
                .total_tokens
                .saturating_add(i64::try_from(usage.total_tokens()).unwrap_or(i64::MAX));
            snapshot.total_cached_tokens = snapshot
                .total_cached_tokens
                .saturating_add(i64::try_from(usage.cached_tokens).unwrap_or(i64::MAX));
            (snapshot.total_tokens, snapshot.total_cached_tokens)
        };
        let _ = self
            .output
            .send_event(RuntimeToServerEvent::UsageTotalsChanged {
                total_tokens,
                total_cached_tokens,
            })
            .await;
    }

    async fn emit(&self, event: StreamEvent) {
        match &self.projection {
            Projection::Main(..) => {
                let runtime_event = match event {
                    StreamEvent::TurnStarted => RuntimeToServerEvent::TurnStarted,
                    StreamEvent::TurnEnded => RuntimeToServerEvent::TurnEnded,
                    StreamEvent::ThinkingDelta { delta } => {
                        RuntimeToServerEvent::ThinkingDelta(delta)
                    }
                    StreamEvent::TextDelta { delta } => RuntimeToServerEvent::TextDelta(delta),
                    StreamEvent::ToolUse { tool_use } => RuntimeToServerEvent::ToolUse(tool_use),
                    StreamEvent::ToolResult { tool_result } => {
                        RuntimeToServerEvent::ToolResult(tool_result)
                    }
                };
                let _ = self.output.send_event(runtime_event).await;
            }
            Projection::Child(child) => {
                let info = &child.info;
                let payload = match event {
                    StreamEvent::TurnStarted => AgentTaskEvent::TurnStarted,
                    StreamEvent::TurnEnded => AgentTaskEvent::TurnEnded,
                    StreamEvent::ThinkingDelta { delta } => AgentTaskEvent::ThinkingDelta { delta },
                    StreamEvent::TextDelta { delta } => AgentTaskEvent::TextDelta { delta },
                    StreamEvent::ToolUse { tool_use } => AgentTaskEvent::ToolUse { tool_use },
                    StreamEvent::ToolResult { tool_result } => {
                        AgentTaskEvent::ToolResult { tool_result }
                    }
                };
                self.emit_task_event(info, payload).await;
            }
        }
    }

    async fn emit_task_event(&self, info: &AgentTaskInfo, payload: AgentTaskEvent) {
        let _ = self
            .output
            .send_event(RuntimeToServerEvent::AgentTaskEvent(
                AgentTaskEventEnvelope {
                    task_id: info.task_id.clone(),
                    thread_id: info.thread_id.clone(),
                    parent_task_id: info.parent_task_id.clone(),
                    owner_thread_id: info.owner_thread_id.clone(),
                    truncated: false,
                    payload,
                },
            ))
            .await;
    }
}
