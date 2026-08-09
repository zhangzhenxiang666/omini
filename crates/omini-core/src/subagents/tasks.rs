use crate::engine::{QueryContext, QueryEngine};
use crate::skills::SkillSummary;
use crate::subagents::{AgentSpec, AgentTaskRequest};
use crate::tools::{
    PendingToolPauses, ToolExecutionContext, ToolRegistry, ToolResult, ToolRuntimeContext,
    create_agent_registry_from_parent,
};
use crate::types::events::EngineToRuntimeEvent;
use chrono::Utc;
use omini_config::project::ThreadDir;
use omini_config::{ProviderProfile, Settings};
use omini_domain::events::{
    ActiveProfile, AgentTaskEvent, AgentTaskEventEnvelope, AgentTaskExecutionMode, AgentTaskInfo,
    AgentTaskResult, AgentTaskStatus, MAX_AGENT_DEPTH, SessionUsageSnapshot, ToolPauseKind,
    ToolPauseResponse,
};
use omini_domain::message::{ContentBlock, Message, Role};
use omini_permissions::PermissionEngine;
use omini_provider_api::{FinishReason, LlmClient};
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::persistence::{RuntimePersistenceEvent, ThreadRecord};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::Instrument;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct AgentTaskCompletion {
    pub(crate) task_id: String,
    pub(crate) agent: String,
    pub(crate) title: String,
    pub(crate) status: AgentTaskStatus,
}

const BACKGROUND_TASK_MEMORY_LIMIT: usize = 30;

struct TaskEntry {
    info: AgentTaskInfo,
    cancelled: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
}

struct PreparedTask {
    info: AgentTaskInfo,
    settings: Arc<Settings>,
    tool_registry: Arc<ToolRegistry>,
    thread_dir: ThreadDir,
    project: omini_config::project::ProjectDir,
    agent_registry: Arc<crate::subagents::AgentRegistry>,
    skill_registry: Arc<crate::skills::SkillRegistry>,
    initial_message: Message,
    llm_context_version: i64,
    warnings: Vec<String>,
    cancelled: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
}

/// 归属于主线程的长期服务，管理后台根 task 及其同步后代。
pub struct AgentTaskSupervisor {
    event_tx: mpsc::Sender<RuntimeToServerEvent>,
    persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
    completion_tx: mpsc::UnboundedSender<AgentTaskCompletion>,
    pending_tool_pauses: PendingToolPauses,
    permission_engine: Arc<PermissionEngine>,
    active_profile: Arc<RwLock<ActiveProfile>>,
    owner_usage: Arc<Mutex<SessionUsageSnapshot>>,
    tasks: Mutex<HashMap<String, TaskEntry>>,
    idle_notify: Notify,
}

impl std::fmt::Debug for AgentTaskSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTaskSupervisor")
            .field(
                "task_count",
                &self.tasks.lock().expect("agent task mutex poisoned").len(),
            )
            .finish_non_exhaustive()
    }
}

impl AgentTaskSupervisor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        event_tx: mpsc::Sender<RuntimeToServerEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        completion_tx: mpsc::UnboundedSender<AgentTaskCompletion>,
        pending_tool_pauses: PendingToolPauses,
        permission_engine: Arc<PermissionEngine>,
        active_profile: Arc<RwLock<ActiveProfile>>,
        owner_usage: Arc<Mutex<SessionUsageSnapshot>>,
        initial_tasks: Vec<AgentTaskInfo>,
    ) -> Arc<Self> {
        let mut initial_tasks = initial_tasks
            .into_iter()
            .filter(|task| {
                task.execution_mode == AgentTaskExecutionMode::Background
                    || !task.status.is_terminal()
            })
            .collect::<Vec<_>>();
        prune_initial_delivered_background_history(&mut initial_tasks);
        let tasks = initial_tasks
            .iter()
            .cloned()
            .map(|info| {
                (
                    info.task_id.clone(),
                    TaskEntry {
                        info,
                        cancelled: Arc::new(AtomicBool::new(false)),
                        cancel_notify: Arc::new(Notify::new()),
                    },
                )
            })
            .collect();
        let supervisor = Arc::new(Self {
            event_tx,
            persistence_tx,
            completion_tx,
            pending_tool_pauses,
            permission_engine,
            active_profile,
            owner_usage,
            tasks: Mutex::new(tasks),
            idle_notify: Notify::new(),
        });
        for task in initial_tasks.into_iter().filter(|task| {
            task.parent_task_id.is_none()
                && task.execution_mode == AgentTaskExecutionMode::Background
                && task.status.is_terminal()
                && !task.notification_delivered
        }) {
            let _ = supervisor.completion_tx.send(AgentTaskCompletion {
                task_id: task.task_id,
                agent: task.agent,
                title: task.title,
                status: task.status,
            });
        }
        supervisor
    }

    pub async fn spawn_background(
        self: &Arc<Self>,
        request: AgentTaskRequest,
        ctx: ToolExecutionContext,
        runtime: Arc<ToolRuntimeContext>,
    ) -> ToolResult {
        if runtime.agent_depth != 0 {
            return ToolResult::error("spawn_agent is only available to the main agent");
        }
        let prepared = match self
            .prepare_task(request, ctx, runtime, AgentTaskExecutionMode::Background)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => return ToolResult::error(error),
        };
        let response = task_status_payload(&prepared.info);
        let supervisor = Arc::clone(self);
        let task_id = prepared.info.task_id.clone();
        tokio::spawn(async move {
            let execution = tokio::spawn({
                let supervisor = Arc::clone(&supervisor);
                async move { supervisor.execute_task(prepared).await }
            });
            if let Err(error) = execution.await {
                let _ = supervisor
                    .finish_panicked_task(&task_id, format!("agent task panicked: {error}"))
                    .await;
            }
        });
        ToolResult::ok(response.to_string())
    }

    pub async fn run_synchronous(
        self: &Arc<Self>,
        request: AgentTaskRequest,
        ctx: ToolExecutionContext,
        runtime: Arc<ToolRuntimeContext>,
    ) -> ToolResult {
        if runtime.agent_depth == 0 || runtime.agent_depth >= MAX_AGENT_DEPTH {
            return ToolResult::error("run_agent is only available to subagents below max depth");
        }
        let prepared = match self
            .prepare_task(request, ctx, runtime, AgentTaskExecutionMode::Synchronous)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => return ToolResult::error(error),
        };
        let task_id = prepared.info.task_id.clone();
        let execution = tokio::spawn({
            let supervisor = Arc::clone(self);
            async move { supervisor.execute_task(prepared).await }
        });
        self.finish_synchronous_execution(&task_id, execution).await
    }

    async fn finish_synchronous_execution(
        &self,
        task_id: &str,
        execution: tokio::task::JoinHandle<AgentTaskInfo>,
    ) -> ToolResult {
        let response = match execution.await {
            Ok(result) => task_result_response(&result),
            Err(error) => {
                let message = format!("agent task panicked: {error}");
                self.finish_panicked_task(task_id, message)
                    .await
                    .map(|info| task_result_response(&info))
                    .unwrap_or_else(|| ToolResult::error(serialization_failure_payload(task_id)))
            }
        };
        self.tasks
            .lock()
            .expect("agent task mutex poisoned")
            .remove(task_id);
        response
    }

    pub fn get_task(&self, task_id: &str) -> ToolResult {
        let tasks = self.tasks.lock().expect("agent task mutex poisoned");
        let Some(task) = tasks.get(task_id) else {
            return ToolResult::error(format!("unknown agent task '{task_id}'"));
        };
        ToolResult::ok(task_result_payload(&task.info))
    }

    pub async fn cancel_task(&self, task_id: &str) -> ToolResult {
        let (response, cancelling_ids, cancelling_thread_ids) = {
            let mut tasks = self.tasks.lock().expect("agent task mutex poisoned");
            let Some(target) = tasks.get(task_id) else {
                return ToolResult::error(format!("unknown agent task '{task_id}'"));
            };
            if target.info.status.is_terminal() {
                return ToolResult::ok(task_status_payload(&target.info));
            }
            let ids = descendant_ids(&tasks, task_id);
            let thread_ids = ids
                .iter()
                .filter_map(|id| tasks.get(id).map(|task| task.info.thread_id.clone()))
                .collect::<Vec<_>>();
            for id in &ids {
                if let Some(task) = tasks.get_mut(id) {
                    task.info.status = AgentTaskStatus::Cancelling;
                    task.info.updated_at = Utc::now();
                    task.cancelled.store(true, Ordering::Relaxed);
                    task.cancel_notify.notify_waiters();
                }
            }
            let response = tasks
                .get(task_id)
                .map(|task| task_status_payload(&task.info))
                .unwrap_or_else(|| serialization_failure_payload(task_id));
            (response, ids, thread_ids)
        };
        self.pending_tool_pauses
            .lock()
            .expect("pending tool pause mutex poisoned")
            .retain(|pause_id, _| {
                !cancelling_thread_ids
                    .iter()
                    .any(|thread_id| pause_id.starts_with(&format!("{thread_id}:")))
            });
        let _ = self
            .persistence_tx
            .send(RuntimePersistenceEvent::SetAgentTasksCancelling {
                task_ids: cancelling_ids,
                updated_at: Utc::now(),
            })
            .await;
        ToolResult::ok(response)
    }

    pub async fn cancel_all(&self) {
        let root_ids = {
            let tasks = self.tasks.lock().expect("agent task mutex poisoned");
            tasks
                .values()
                .filter(|entry| {
                    entry.info.parent_task_id.is_none() && !entry.info.status.is_terminal()
                })
                .map(|entry| entry.info.task_id.clone())
                .collect::<Vec<_>>()
        };
        for task_id in root_ids {
            let _ = self.cancel_task(&task_id).await;
        }
    }

    pub fn has_active_tasks(&self) -> bool {
        self.tasks
            .lock()
            .expect("agent task mutex poisoned")
            .values()
            .any(|entry| !entry.info.status.is_terminal())
    }

    pub(crate) fn mark_notifications_delivered(&self, task_ids: &[String]) {
        let mut tasks = self.tasks.lock().expect("agent task mutex poisoned");
        for task_id in task_ids {
            if let Some(task) = tasks.get_mut(task_id) {
                task.info.notification_delivered = true;
            }
        }
        prune_delivered_background_history(&mut tasks);
    }

    pub async fn wait_until_idle(&self) {
        while self.has_active_tasks() {
            self.idle_notify.notified().await;
        }
    }

    async fn prepare_task(
        self: &Arc<Self>,
        request: AgentTaskRequest,
        ctx: ToolExecutionContext,
        runtime: Arc<ToolRuntimeContext>,
        execution_mode: AgentTaskExecutionMode,
    ) -> Result<PreparedTask, String> {
        let depth = runtime.agent_depth.saturating_add(1);
        if depth > MAX_AGENT_DEPTH {
            return Err(format!("maximum agent depth is {MAX_AGENT_DEPTH}"));
        }
        let name = request.name.trim();
        let Some(spec) = runtime.agent_registry.get(name).cloned() else {
            return Err(unknown_agent_message(name, &runtime));
        };
        let (tool_registry, mut warnings) = create_agent_registry_from_parent(
            &ctx.tool_registry,
            spec.tool_policy.allow.as_deref(),
            spec.tool_policy.deny.as_deref().unwrap_or(&[]),
            depth,
        )?;
        let (settings, model_warnings) = resolve_agent_settings(&ctx.settings, &spec);
        warnings.extend(model_warnings);
        let mut settings = settings;
        settings.system_prompt = Some(agent_system_prompt(
            &ctx.settings,
            &spec,
            &agent_skill_summaries(&tool_registry, &runtime.skill_registry),
            depth,
        ));
        let settings = Arc::new(settings);

        let task_id = Uuid::new_v4().to_string();
        let thread_id = Uuid::new_v4().to_string();
        let thread_dir = runtime
            .project
            .create_thread(&thread_id)
            .map_err(|error| format!("failed to create agent thread: {error}"))?;
        let now = Utc::now();
        let initial_context_version = 1;
        let info = AgentTaskInfo {
            task_id: task_id.clone(),
            thread_id: thread_id.clone(),
            parent_task_id: runtime.task_id.clone(),
            owner_thread_id: runtime.owner_thread_id.clone(),
            parent_thread_id: runtime.thread_id.clone(),
            spawn_tool_use_id: ctx.tool_use_id.clone(),
            agent: spec.name.clone(),
            title: request.title,
            depth,
            execution_mode,
            status: AgentTaskStatus::Running,
            result: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
            notification_delivered: false,
        };
        let thread = ThreadRecord {
            id: thread_id,
            parent_thread_id: Some(runtime.thread_id.clone()),
            spawn_tool_use_id: Some(ctx.tool_use_id),
            thread_type: "agent".to_string(),
            agent_label: Some(spec.name),
            provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(|effort| effort.to_string()),
            title: Some(info.title.clone()),
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            llm_context_version: initial_context_version,
            created_at: now,
            updated_at: now,
        };
        let initial_message = Message::from_user_text(request.prompt);
        let (ack_tx, ack_rx) = oneshot::channel();
        let creation_result = self
            .persistence_tx
            .send(RuntimePersistenceEvent::CreateAgentTask {
                task: Box::new(info.clone()),
                thread,
                initial_message: initial_message.clone(),
                ack: ack_tx,
            })
            .await
            .map_err(|_| "agent task persistence channel closed".to_string());
        let creation_result = match creation_result {
            Ok(()) => ack_rx
                .await
                .map_err(|_| "agent task creation acknowledgement dropped".to_string())
                .and_then(|result| result),
            Err(error) => Err(error),
        };
        if let Err(error) = creation_result {
            if let Err(cleanup_error) = std::fs::remove_dir_all(thread_dir.path()) {
                tracing::warn!(
                    path = %thread_dir.path().display(),
                    %cleanup_error,
                    "failed to clean up uncommitted agent thread directory"
                );
            }
            return Err(error);
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_notify = Arc::new(Notify::new());
        self.tasks
            .lock()
            .expect("agent task mutex poisoned")
            .insert(
                task_id,
                TaskEntry {
                    info: info.clone(),
                    cancelled: Arc::clone(&cancelled),
                    cancel_notify: Arc::clone(&cancel_notify),
                },
            );
        self.emit(
            &info,
            AgentTaskEvent::Started {
                parent_thread_id: info.parent_thread_id.clone(),
                spawn_tool_use_id: info.spawn_tool_use_id.clone(),
                agent: info.agent.clone(),
                title: info.title.clone(),
                depth,
                execution_mode,
            },
        )
        .await;
        self.emit(
            &info,
            AgentTaskEvent::MessageCommitted {
                message: initial_message.clone(),
                persist_llm_history: true,
            },
        )
        .await;

        Ok(PreparedTask {
            info,
            settings,
            tool_registry: Arc::new(tool_registry),
            thread_dir,
            project: runtime.project.clone(),
            agent_registry: Arc::clone(&runtime.agent_registry),
            skill_registry: Arc::clone(&runtime.skill_registry),
            initial_message,
            llm_context_version: initial_context_version,
            warnings,
            cancelled,
            cancel_notify,
        })
    }

    async fn execute_task(self: Arc<Self>, prepared: PreparedTask) -> AgentTaskInfo {
        let PreparedTask {
            info,
            settings,
            tool_registry,
            thread_dir,
            project,
            agent_registry,
            skill_registry,
            initial_message,
            llm_context_version,
            mut warnings,
            cancelled,
            cancel_notify,
        } = prepared;
        let task_span = tracing::debug_span!(
            "agent_task",
            task_id = %info.task_id,
            thread_id = %info.thread_id,
            parent_task_id = ?info.parent_task_id,
            owner_thread_id = %info.owner_thread_id,
            depth = info.depth,
            execution_mode = info.execution_mode.as_str(),
            agent = %info.agent,
        );
        let llm_client = LlmClient::new(
            settings.endpoint,
            settings.api_key.clone(),
            settings.base_url.clone(),
        );
        let runtime = Arc::new(ToolRuntimeContext {
            thread_id: info.thread_id.clone(),
            run_id: None,
            thread_type: "agent".to_string(),
            agent_label: Some(info.agent.clone()),
            thread_dir,
            llm_context_version: Arc::new(std::sync::atomic::AtomicI64::new(llm_context_version)),
            agent_depth: info.depth,
            task_id: Some(info.task_id.clone()),
            owner_thread_id: info.owner_thread_id.clone(),
            agent_registry,
            skill_registry,
            task_supervisor: Some(Arc::clone(&self)),
            project,
        });
        let mut messages = vec![initial_message];
        let (child_tx, child_rx) = mpsc::channel(256);
        let bridge = {
            let supervisor = Arc::clone(&self);
            let bridge_info = info.clone();
            let model_ref = format!("{}/{}", settings.active_provider, settings.model);
            tokio::spawn(async move {
                supervisor
                    .bridge_engine_events(child_rx, &bridge_info, &model_ref)
                    .await
            })
        };
        let engine = QueryEngine::with_shared_tool_controls(
            Arc::clone(&self.pending_tool_pauses),
            Arc::clone(&self.permission_engine),
            Arc::clone(&cancel_notify),
        );
        let result = engine
            .run_query(
                QueryContext {
                    messages: &mut messages,
                    settings: Arc::clone(&settings),
                    llm_client,
                    tool_registry,
                    active_profile: Arc::clone(&self.active_profile),
                    runtime_context: Some(runtime),
                    requires_internal_input: false,
                },
                child_tx,
                Arc::clone(&cancelled),
            )
            .instrument(task_span)
            .await;
        match bridge.await {
            Ok(bridge_warnings) => warnings.extend(bridge_warnings),
            Err(error) => warnings.push(format!("agent event bridge failed: {error}")),
        }
        let status = if cancelled.load(Ordering::Relaxed) {
            AgentTaskStatus::Cancelled
        } else if matches!(result.finish_reason, FinishReason::Error(_)) {
            AgentTaskStatus::Failed
        } else {
            AgentTaskStatus::Completed
        };
        let task_result = AgentTaskResult {
            output: extract_final_text(&messages),
            error: match result.finish_reason {
                FinishReason::Error(error) => Some(error),
                _ => None,
            },
            warnings,
        };
        self.finish_task(&info.task_id, status, task_result).await
    }

    async fn bridge_engine_events(
        &self,
        mut rx: mpsc::Receiver<EngineToRuntimeEvent>,
        info: &AgentTaskInfo,
        model_ref: &str,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                EngineToRuntimeEvent::TurnStarted => {
                    self.emit(info, AgentTaskEvent::TurnStarted).await
                }
                EngineToRuntimeEvent::TurnEnded => self.emit(info, AgentTaskEvent::TurnEnded).await,
                EngineToRuntimeEvent::ThinkingDelta(delta) => {
                    self.emit(info, AgentTaskEvent::ThinkingDelta { delta })
                        .await
                }
                EngineToRuntimeEvent::TextDelta(delta) => {
                    self.emit(info, AgentTaskEvent::TextDelta { delta }).await
                }
                EngineToRuntimeEvent::ToolUse(tool_use) => {
                    self.emit(info, AgentTaskEvent::ToolUse { tool_use }).await
                }
                EngineToRuntimeEvent::ToolResult(tool_result) => {
                    self.emit(info, AgentTaskEvent::ToolResult { tool_result })
                        .await
                }
                EngineToRuntimeEvent::UserMessageProduced { message, .. }
                | EngineToRuntimeEvent::MessageProduced(message)
                | EngineToRuntimeEvent::ToolResultsProduced(message) => {
                    if let Err(error) = self
                        .persist_agent_message(info, message, Some(model_ref), true, true)
                        .await
                    {
                        warnings.push(error);
                    }
                }
                EngineToRuntimeEvent::AgentTaskNotificationsProduced { ack, .. } => {
                    let _ = ack.send(Err(
                        "agent task notifications are only supported by the main engine"
                            .to_string(),
                    ));
                }
                EngineToRuntimeEvent::ToolResultsDisplayProduced(message) => {
                    if let Err(error) = self
                        .persist_agent_message(info, message, Some(model_ref), false, true)
                        .await
                    {
                        warnings.push(error);
                    }
                }
                EngineToRuntimeEvent::LlmHistoryProduced(message) => {
                    if let Err(error) = self
                        .persist_agent_message(info, message, None, true, false)
                        .await
                    {
                        warnings.push(error);
                    }
                }
                EngineToRuntimeEvent::ReplaceLlmContext {
                    thread_id,
                    expected_version,
                    messages,
                    ack,
                } => {
                    let _ = self
                        .persistence_tx
                        .send(RuntimePersistenceEvent::ReplaceLlmContext {
                            thread_id,
                            expected_version,
                            messages,
                            created_at: Utc::now(),
                            ack,
                        })
                        .await;
                }
                EngineToRuntimeEvent::ToolPauseRequested(request) => {
                    let active_profile = *self
                        .active_profile
                        .read()
                        .expect("active profile lock poisoned");
                    if active_profile == ActiveProfile::Auto
                        && matches!(request.kind, ToolPauseKind::Permission(_))
                    {
                        let resolver = crate::engine::ToolPauseResolver::new(Arc::clone(
                            &self.pending_tool_pauses,
                        ));
                        if let Err(error) = resolver.resolve_tool_pause(
                            &request.tool_use_id,
                            ToolPauseResponse::Permission {
                                approved: true,
                                note: None,
                            },
                        ) {
                            warnings.push(error.to_string());
                        }
                        continue;
                    }
                    let _ = self
                        .event_tx
                        .send(RuntimeToServerEvent::ToolPauseRequested(*request))
                        .await;
                }
                EngineToRuntimeEvent::UsageRecorded(usage) => {
                    let _ = self
                        .persistence_tx
                        .send(RuntimePersistenceEvent::RecordThreadUsage {
                            thread_id: info.thread_id.clone(),
                            usage,
                        })
                        .await;
                    self.record_owner_agent_usage(info, usage).await;
                }
                EngineToRuntimeEvent::Warning(warning) => warnings.push(warning),
                EngineToRuntimeEvent::Error(error) => warnings.push(error),
                EngineToRuntimeEvent::CompactShrinkStarted(_)
                | EngineToRuntimeEvent::CompactShrinkFinished(_)
                | EngineToRuntimeEvent::CompactShrinkFailed(_)
                | EngineToRuntimeEvent::CompactSummaryStarted(_)
                | EngineToRuntimeEvent::CompactSummaryDelta(_)
                | EngineToRuntimeEvent::CompactSummaryFinished(_)
                | EngineToRuntimeEvent::CompactSummaryFailed(_) => {}
                EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage) => {
                    let _ = self
                        .persistence_tx
                        .send(RuntimePersistenceEvent::RecordThreadTotalUsage {
                            thread_id: info.thread_id.clone(),
                            usage,
                        })
                        .await;
                    self.record_owner_agent_usage(info, usage).await;
                }
            }
        }
        warnings
    }

    async fn record_owner_agent_usage(
        &self,
        info: &AgentTaskInfo,
        usage: omini_domain::usage::Usage,
    ) {
        let _ = self
            .persistence_tx
            .send(RuntimePersistenceEvent::RecordOwnerAgentUsage {
                thread_id: info.owner_thread_id.clone(),
                usage,
            })
            .await;
        let (total_tokens, total_cached_tokens) = {
            let mut snapshot = self.owner_usage.lock().expect("owner usage lock poisoned");
            snapshot.total_tokens = snapshot
                .total_tokens
                .saturating_add(i64::try_from(usage.total_tokens()).unwrap_or(i64::MAX));
            snapshot.total_cached_tokens = snapshot
                .total_cached_tokens
                .saturating_add(i64::try_from(usage.cached_tokens).unwrap_or(i64::MAX));
            (snapshot.total_tokens, snapshot.total_cached_tokens)
        };
        let _ = self
            .event_tx
            .send(RuntimeToServerEvent::UsageTotalsChanged {
                total_tokens,
                total_cached_tokens,
            })
            .await;
    }

    async fn persist_agent_message(
        &self,
        info: &AgentTaskInfo,
        message: Message,
        model_ref: Option<&str>,
        persist_llm_history: bool,
        display_in_ui: bool,
    ) -> Result<(), String> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.persistence_tx
            .send(RuntimePersistenceEvent::PersistAgentMessage {
                thread_id: info.thread_id.clone(),
                message: message.clone(),
                model_ref: (message.role == Role::Assistant)
                    .then(|| model_ref.map(str::to_string))
                    .flatten(),
                persist_llm_history,
                display_in_ui,
                created_at: Utc::now(),
                ack: ack_tx,
            })
            .await
            .map_err(|_| "agent message persistence channel closed".to_string())?;
        ack_rx
            .await
            .map_err(|_| "agent message acknowledgement dropped".to_string())??;
        if display_in_ui {
            self.emit(
                info,
                AgentTaskEvent::MessageCommitted {
                    message,
                    persist_llm_history,
                },
            )
            .await;
        }
        Ok(())
    }

    async fn finish_task(
        &self,
        task_id: &str,
        status: AgentTaskStatus,
        result: AgentTaskResult,
    ) -> AgentTaskInfo {
        let completed_at = Utc::now();
        let (ack_tx, ack_rx) = oneshot::channel();
        let persistence_result = self
            .persistence_tx
            .send(RuntimePersistenceEvent::FinishAgentTask {
                task_id: task_id.to_string(),
                status,
                result: result.clone(),
                completed_at,
                ack: ack_tx,
            })
            .await
            .map_err(|_| "agent task persistence channel closed".to_string());
        let persistence_result = match persistence_result {
            Ok(()) => ack_rx
                .await
                .map_err(|_| "agent task finish acknowledgement dropped".to_string())
                .and_then(|result| result),
            Err(error) => Err(error),
        };
        let (info, notify_owner) = {
            let mut tasks = self.tasks.lock().expect("agent task mutex poisoned");
            let entry = tasks
                .get_mut(task_id)
                .expect("finishing agent task must exist");
            let final_status = if persistence_result.is_ok() {
                status
            } else {
                AgentTaskStatus::Failed
            };
            let mut final_result = result;
            if let Err(error) = persistence_result {
                final_result.error = Some(error);
            }
            entry.info.status = final_status;
            entry.info.result = Some(final_result);
            entry.info.updated_at = completed_at;
            entry.info.completed_at = Some(completed_at);
            (
                entry.info.clone(),
                entry.info.execution_mode == AgentTaskExecutionMode::Background
                    && entry.info.parent_task_id.is_none(),
            )
        };
        self.emit(
            &info,
            AgentTaskEvent::Finished {
                status: info.status,
                result: info.result.clone(),
            },
        )
        .await;
        if notify_owner {
            let _ = self.completion_tx.send(AgentTaskCompletion {
                task_id: info.task_id.clone(),
                agent: info.agent.clone(),
                title: info.title.clone(),
                status: info.status,
            });
        }
        self.idle_notify.notify_one();
        info
    }

    async fn finish_panicked_task(&self, task_id: &str, error: String) -> Option<AgentTaskInfo> {
        let should_finish = self
            .tasks
            .lock()
            .expect("agent task mutex poisoned")
            .get(task_id)
            .is_some_and(|entry| !entry.info.status.is_terminal());
        if should_finish {
            return Some(
                self.finish_task(
                    task_id,
                    AgentTaskStatus::Failed,
                    AgentTaskResult {
                        output: None,
                        error: Some(error),
                        warnings: Vec::new(),
                    },
                )
                .await,
            );
        }
        None
    }

    async fn emit(&self, info: &AgentTaskInfo, payload: AgentTaskEvent) {
        let _ = self
            .event_tx
            .send(RuntimeToServerEvent::AgentTaskEvent(
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

fn prune_initial_delivered_background_history(tasks: &mut Vec<AgentTaskInfo>) {
    let mut delivered = tasks
        .iter()
        .filter(|task| is_prunable_delivered_background(task))
        .map(|task| (task.updated_at, task.task_id.clone()))
        .collect::<Vec<_>>();
    if delivered.len() <= BACKGROUND_TASK_MEMORY_LIMIT {
        return;
    }
    delivered.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    let remove_ids = delivered
        .into_iter()
        .skip(BACKGROUND_TASK_MEMORY_LIMIT)
        .map(|(_, task_id)| task_id)
        .collect::<HashSet<_>>();
    tasks.retain(|task| !remove_ids.contains(&task.task_id));
}

fn prune_delivered_background_history(tasks: &mut HashMap<String, TaskEntry>) {
    let mut delivered = tasks
        .values()
        .filter(|entry| is_prunable_delivered_background(&entry.info))
        .map(|entry| (entry.info.updated_at, entry.info.task_id.clone()))
        .collect::<Vec<_>>();
    if delivered.len() <= BACKGROUND_TASK_MEMORY_LIMIT {
        return;
    }
    delivered.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    for (_, task_id) in delivered.into_iter().skip(BACKGROUND_TASK_MEMORY_LIMIT) {
        tasks.remove(&task_id);
    }
}

fn is_prunable_delivered_background(task: &AgentTaskInfo) -> bool {
    task.execution_mode == AgentTaskExecutionMode::Background
        && task.status.is_terminal()
        && task.notification_delivered
}

fn descendant_ids(tasks: &HashMap<String, TaskEntry>, task_id: &str) -> Vec<String> {
    let mut ids = vec![task_id.to_string()];
    let mut cursor = 0;
    while cursor < ids.len() {
        let parent = ids[cursor].clone();
        ids.extend(
            tasks
                .values()
                .filter(|entry| entry.info.parent_task_id.as_deref() == Some(&parent))
                .map(|entry| entry.info.task_id.clone()),
        );
        cursor += 1;
    }
    ids
}

#[derive(Serialize)]
struct AgentTaskStatusResponse<'a> {
    task_id: &'a str,
    status: AgentTaskStatus,
}

#[derive(Serialize)]
struct AgentTaskResultResponse<'a> {
    task_id: &'a str,
    status: AgentTaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a AgentTaskResult>,
}

fn task_status_payload(info: &AgentTaskInfo) -> String {
    serde_json::to_string(&AgentTaskStatusResponse {
        task_id: &info.task_id,
        status: info.status,
    })
    .unwrap_or_else(|_| serialization_failure_payload(&info.task_id))
}

fn task_result_payload(info: &AgentTaskInfo) -> String {
    serde_json::to_string(&AgentTaskResultResponse {
        task_id: &info.task_id,
        status: info.status,
        result: info.result.as_ref(),
    })
    .unwrap_or_else(|_| serialization_failure_payload(&info.task_id))
}

fn serialization_failure_payload(task_id: &str) -> String {
    format!(r#"{{"task_id":"{task_id}","status":"failed"}}"#)
}

fn task_result_response(info: &AgentTaskInfo) -> ToolResult {
    // warnings 保留给调用模型，用于判断模型回退和持久化异常是否影响结果可靠性。
    let payload = task_result_payload(info);
    if info.status == AgentTaskStatus::Completed {
        ToolResult::ok(payload)
    } else {
        ToolResult::error(payload)
    }
}

fn unknown_agent_message(name: &str, runtime: &ToolRuntimeContext) -> String {
    let available = runtime.agent_registry.sorted_names();
    let mut message = format!(
        "unknown agent '{name}'. Available agents: {}",
        available.join(", ")
    );
    if !runtime.agent_registry.diagnostics.is_empty() {
        message.push_str("\n\nAgent load warnings:");
        for diagnostic in &runtime.agent_registry.diagnostics {
            message.push_str("\n- ");
            message.push_str(diagnostic.message());
        }
    }
    message
}

fn resolve_agent_settings(parent_settings: &Settings, spec: &AgentSpec) -> (Settings, Vec<String>) {
    let mut settings = parent_settings.clone();
    let mut warnings = Vec::new();
    let Some(model_spec) = &spec.model else {
        return (settings, warnings);
    };
    let Some(provider) = parent_settings.providers.get(&model_spec.provider) else {
        warnings.push(format!(
            "provider '{}' is not configured; falling back to {}/{}",
            model_spec.provider, parent_settings.active_provider, parent_settings.model
        ));
        return (settings, warnings);
    };
    if !provider
        .models
        .iter()
        .any(|model| model.id == model_spec.model)
    {
        warnings.push(format!(
            "model '{}' is not configured for provider '{}'; falling back to {}/{}",
            model_spec.model,
            model_spec.provider,
            parent_settings.active_provider,
            parent_settings.model
        ));
        return (settings, warnings);
    }
    apply_provider(
        &mut settings,
        &model_spec.provider,
        &model_spec.model,
        provider,
    );
    (settings, warnings)
}

fn apply_provider(
    settings: &mut Settings,
    provider_name: &str,
    model_name: &str,
    provider: &ProviderProfile,
) {
    settings.active_provider = provider_name.to_string();
    settings.model = model_name.to_string();
    settings.endpoint = provider.endpoint;
    settings.api_key = provider.api_key.clone();
    settings.base_url = provider.base_url.clone();
}

fn agent_system_prompt(
    parent: &Settings,
    spec: &AgentSpec,
    skills: &[SkillSummary],
    depth: u8,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are running as an isolated agent task for Omini.\n\n");
    if let Some(section) = crate::prompts::language_preference_section(parent) {
        prompt.push_str(&section);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&crate::prompts::project_context_prompt(&parent.cwd));
    if let Some(section) = crate::prompts::skill_section(skills) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    prompt.push_str("\n\n<agent_instructions>\n");
    if depth < MAX_AGENT_DEPTH {
        prompt.push_str(
            "Return a concise final result for the parent agent. You may use run_agent for one synchronous child level when useful.\n\n",
        );
    } else {
        prompt.push_str("Return a concise final result for the parent agent.\n\n");
    }
    prompt.push_str("<agent>\n  <name>");
    prompt.push_str(&spec.name);
    prompt.push_str("</name>\n  <description>");
    prompt.push_str(&spec.description);
    prompt.push_str("</description>\n</agent>\n\n");
    prompt.push_str(&spec.instructions);
    prompt.push_str("\n</agent_instructions>");
    prompt
}

fn agent_skill_summaries(
    tool_registry: &ToolRegistry,
    skill_registry: &crate::skills::SkillRegistry,
) -> Vec<SkillSummary> {
    if tool_registry.contains("skill") {
        skill_registry.injected_summaries()
    } else {
        Vec::new()
    }
}

fn extract_final_text(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        (message.role == Role::Assistant).then(|| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
    })
}

#[cfg(test)]
mod tests {
    use crate::tools::{PendingToolPause, PendingToolPauses};
    use omini_domain::events::{PermissionPreview, SessionUsageSnapshot, ToolPauseRequest};
    use omini_domain::message::{ToolResultBlock, ToolUseBlock};
    use omini_domain::usage::Usage;
    use std::collections::HashMap;

    use super::*;

    fn task_info(depth: u8) -> AgentTaskInfo {
        let now = Utc::now();
        AgentTaskInfo {
            task_id: format!("task_{depth}"),
            thread_id: format!("thread_{depth}"),
            parent_task_id: (depth > 1).then(|| "task_1".to_string()),
            owner_thread_id: "owner".to_string(),
            parent_thread_id: "parent".to_string(),
            spawn_tool_use_id: format!("spawn_{depth}"),
            agent: "general".to_string(),
            title: "Test".to_string(),
            depth,
            execution_mode: if depth == 1 {
                AgentTaskExecutionMode::Background
            } else {
                AgentTaskExecutionMode::Synchronous
            },
            status: AgentTaskStatus::Running,
            result: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
            notification_delivered: false,
        }
    }

    #[allow(clippy::type_complexity)]
    fn test_supervisor(
        initial_tasks: Vec<AgentTaskInfo>,
    ) -> (
        Arc<AgentTaskSupervisor>,
        mpsc::Receiver<RuntimeToServerEvent>,
        mpsc::Receiver<RuntimePersistenceEvent>,
        PendingToolPauses,
        Arc<RwLock<ActiveProfile>>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (persistence_tx, persistence_rx) = mpsc::channel(32);
        let (completion_tx, _completion_rx) = mpsc::unbounded_channel();
        let pending_pauses: PendingToolPauses = Arc::new(Mutex::new(HashMap::new()));
        let active_profile = Arc::new(RwLock::new(ActiveProfile::Main));
        let supervisor = AgentTaskSupervisor::new(
            event_tx,
            persistence_tx,
            completion_tx,
            Arc::clone(&pending_pauses),
            Arc::new(PermissionEngine::empty("/tmp")),
            Arc::clone(&active_profile),
            Arc::new(Mutex::new(SessionUsageSnapshot::default())),
            initial_tasks,
        );
        (
            supervisor,
            event_rx,
            persistence_rx,
            pending_pauses,
            active_profile,
        )
    }

    #[test]
    fn model_visible_task_payloads_hide_internal_fields_and_empty_warnings() {
        let mut info = task_info(1);
        info.status = AgentTaskStatus::Completed;
        info.result = Some(AgentTaskResult {
            output: Some("done".to_string()),
            error: None,
            warnings: Vec::new(),
        });

        let status: serde_json::Value = serde_json::from_str(&task_status_payload(&info)).unwrap();
        assert_eq!(
            status.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["status", "task_id"]
        );

        let result: serde_json::Value = serde_json::from_str(&task_result_payload(&info)).unwrap();
        assert_eq!(
            result.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["result", "status", "task_id"]
        );
        assert_eq!(result["result"]["output"], "done");
        assert!(result["result"].get("warnings").is_none());
        for hidden in [
            "thread_id",
            "parent_task_id",
            "owner_thread_id",
            "created_at",
            "execution_mode",
            "notification_delivered",
        ] {
            assert!(result.get(hidden).is_none(), "unexpected field {hidden}");
        }

        info.result.as_mut().unwrap().warnings = vec!["fallback model used".to_string()];
        let result: serde_json::Value = serde_json::from_str(&task_result_payload(&info)).unwrap();
        assert_eq!(result["result"]["warnings"][0], "fallback model used");
    }

    #[tokio::test]
    async fn terminal_cancel_is_idempotent_and_returns_only_status() {
        let mut info = task_info(1);
        info.status = AgentTaskStatus::Failed;
        let (supervisor, _event_rx, _persistence_rx, _pending_pauses, _active_profile) =
            test_supervisor(vec![info]);

        let response = supervisor.cancel_task("task_1").await;

        assert!(!response.is_error);
        let payload: serde_json::Value = serde_json::from_str(&response.output).unwrap();
        assert_eq!(payload["task_id"], "task_1");
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload.as_object().unwrap().len(), 2);
    }

    #[test]
    fn recovery_keeps_background_history_but_drops_terminal_synchronous_tasks() {
        let mut background = task_info(1);
        background.status = AgentTaskStatus::Completed;
        background.notification_delivered = true;
        let mut synchronous = task_info(2);
        synchronous.status = AgentTaskStatus::Failed;
        let (supervisor, _event_rx, _persistence_rx, _pending_pauses, _active_profile) =
            test_supervisor(vec![background, synchronous]);

        assert!(!supervisor.get_task("task_1").is_error);
        assert!(supervisor.get_task("task_2").is_error);
    }

    #[test]
    fn recovery_keeps_recent_delivered_background_history_limit() {
        let now = Utc::now();
        let mut tasks = Vec::new();
        for index in 0..35 {
            let mut task = task_info(1);
            task.task_id = format!("done_{index:02}");
            task.thread_id = format!("thread_done_{index:02}");
            task.spawn_tool_use_id = format!("spawn_done_{index:02}");
            task.status = AgentTaskStatus::Completed;
            task.notification_delivered = true;
            task.updated_at = now + chrono::Duration::seconds(i64::from(index));
            task.completed_at = Some(task.updated_at);
            tasks.push(task);
        }

        let (supervisor, _event_rx, _persistence_rx, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        for index in 0..5 {
            assert!(supervisor.get_task(&format!("done_{index:02}")).is_error);
        }
        for index in 5..35 {
            assert!(!supervisor.get_task(&format!("done_{index:02}")).is_error);
        }
    }

    #[test]
    fn recovery_keeps_running_and_undelivered_background_tasks_beyond_limit() {
        let now = Utc::now();
        let mut tasks = Vec::new();
        for index in 0..35 {
            let mut task = task_info(1);
            task.task_id = format!("done_{index:02}");
            task.thread_id = format!("thread_done_{index:02}");
            task.spawn_tool_use_id = format!("spawn_done_{index:02}");
            task.status = AgentTaskStatus::Completed;
            task.notification_delivered = true;
            task.updated_at = now + chrono::Duration::seconds(i64::from(index));
            task.completed_at = Some(task.updated_at);
            tasks.push(task);
        }
        let mut running = task_info(1);
        running.task_id = "running_old".to_string();
        running.thread_id = "thread_running_old".to_string();
        running.spawn_tool_use_id = "spawn_running_old".to_string();
        running.updated_at = now - chrono::Duration::seconds(100);
        tasks.push(running);
        let mut undelivered = task_info(1);
        undelivered.task_id = "undelivered_old".to_string();
        undelivered.thread_id = "thread_undelivered_old".to_string();
        undelivered.spawn_tool_use_id = "spawn_undelivered_old".to_string();
        undelivered.status = AgentTaskStatus::Completed;
        undelivered.notification_delivered = false;
        undelivered.updated_at = now - chrono::Duration::seconds(101);
        undelivered.completed_at = Some(undelivered.updated_at);
        tasks.push(undelivered);

        let (supervisor, _event_rx, _persistence_rx, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        assert!(!supervisor.get_task("running_old").is_error);
        assert!(!supervisor.get_task("undelivered_old").is_error);
        assert!(supervisor.get_task("done_00").is_error);
    }

    #[test]
    fn mark_notifications_delivered_prunes_old_delivered_background_tasks() {
        let now = Utc::now();
        let mut tasks = Vec::new();
        let mut delivered_ids = Vec::new();
        for index in 0..32 {
            let mut task = task_info(1);
            task.task_id = format!("task_{index:02}");
            task.thread_id = format!("thread_{index:02}");
            task.spawn_tool_use_id = format!("spawn_{index:02}");
            task.status = AgentTaskStatus::Completed;
            task.notification_delivered = false;
            task.updated_at = now + chrono::Duration::seconds(i64::from(index));
            task.completed_at = Some(task.updated_at);
            delivered_ids.push(task.task_id.clone());
            tasks.push(task);
        }

        let (supervisor, _event_rx, _persistence_rx, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        supervisor.mark_notifications_delivered(&delivered_ids);

        assert!(supervisor.get_task("task_00").is_error);
        assert!(supervisor.get_task("task_01").is_error);
        assert!(!supervisor.get_task("task_02").is_error);
        assert!(!supervisor.get_task("task_31").is_error);
    }

    #[test]
    fn mark_notifications_delivered_does_not_prune_running_or_undelivered_tasks() {
        let now = Utc::now();
        let mut tasks = Vec::new();
        let mut delivered_ids = Vec::new();
        for index in 0..31 {
            let mut task = task_info(1);
            task.task_id = format!("done_{index:02}");
            task.thread_id = format!("thread_done_{index:02}");
            task.spawn_tool_use_id = format!("spawn_done_{index:02}");
            task.status = AgentTaskStatus::Completed;
            task.notification_delivered = false;
            task.updated_at = now + chrono::Duration::seconds(i64::from(index));
            task.completed_at = Some(task.updated_at);
            delivered_ids.push(task.task_id.clone());
            tasks.push(task);
        }
        let mut running = task_info(1);
        running.task_id = "running_old".to_string();
        running.thread_id = "thread_running_old".to_string();
        running.spawn_tool_use_id = "spawn_running_old".to_string();
        running.updated_at = now - chrono::Duration::seconds(100);
        tasks.push(running);
        let mut undelivered = task_info(1);
        undelivered.task_id = "undelivered_old".to_string();
        undelivered.thread_id = "thread_undelivered_old".to_string();
        undelivered.spawn_tool_use_id = "spawn_undelivered_old".to_string();
        undelivered.status = AgentTaskStatus::Completed;
        undelivered.notification_delivered = false;
        undelivered.updated_at = now - chrono::Duration::seconds(101);
        undelivered.completed_at = Some(undelivered.updated_at);
        tasks.push(undelivered);

        let (supervisor, _event_rx, _persistence_rx, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        supervisor.mark_notifications_delivered(&delivered_ids);

        assert!(!supervisor.get_task("running_old").is_error);
        assert!(!supervisor.get_task("undelivered_old").is_error);
        assert!(supervisor.get_task("done_00").is_error);
    }

    #[tokio::test]
    async fn synchronous_terminal_and_panicked_executions_are_removed() {
        for status in [AgentTaskStatus::Completed, AgentTaskStatus::Failed] {
            let initial = task_info(2);
            let task_id = initial.task_id.clone();
            let (supervisor, _event_rx, _persistence_rx, _pending_pauses, _active_profile) =
                test_supervisor(vec![initial.clone()]);
            let mut finished = initial;
            finished.status = status;
            finished.result = Some(AgentTaskResult {
                output: (status == AgentTaskStatus::Completed).then(|| "done".to_string()),
                error: (status == AgentTaskStatus::Failed).then(|| "failed".to_string()),
                warnings: Vec::new(),
            });

            let response = supervisor
                .finish_synchronous_execution(&task_id, tokio::spawn(async move { finished }))
                .await;

            assert_eq!(response.is_error, status != AgentTaskStatus::Completed);
            let payload: serde_json::Value = serde_json::from_str(&response.output).unwrap();
            assert_eq!(payload["task_id"], task_id);
            assert_eq!(payload["status"], status.as_str());
            assert!(supervisor.get_task(&task_id).is_error);
        }

        let initial = task_info(2);
        let task_id = initial.task_id.clone();
        let (supervisor, _event_rx, mut persistence_rx, _pending_pauses, _active_profile) =
            test_supervisor(vec![initial]);
        let persistence = tokio::spawn(async move {
            let Some(RuntimePersistenceEvent::FinishAgentTask { ack, .. }) =
                persistence_rx.recv().await
            else {
                panic!("expected finish persistence event");
            };
            ack.send(Ok(())).unwrap();
        });
        let response = supervisor
            .finish_synchronous_execution(
                &task_id,
                tokio::spawn(async move { panic!("synthetic task panic") }),
            )
            .await;

        persistence.await.unwrap();
        assert!(response.is_error);
        let payload: serde_json::Value = serde_json::from_str(&response.output).unwrap();
        assert_eq!(payload["task_id"], task_id);
        assert_eq!(payload["status"], "failed");
        assert!(
            payload["result"]["error"]
                .as_str()
                .unwrap()
                .contains("synthetic task panic")
        );
        assert!(supervisor.get_task(&task_id).is_error);
    }

    #[tokio::test]
    async fn agent_bridge_reads_current_profile_for_each_permission_request() {
        let (supervisor, mut event_rx, _persistence_rx, pending_pauses, active_profile) =
            test_supervisor(Vec::new());
        let (engine_tx, engine_rx) = mpsc::channel(4);
        let info = task_info(1);
        let bridge = {
            let supervisor = Arc::clone(&supervisor);
            let info = info.clone();
            tokio::spawn(async move {
                supervisor
                    .bridge_engine_events(engine_rx, &info, "openai/test")
                    .await
            })
        };

        for (index, profile) in [
            ActiveProfile::Auto,
            ActiveProfile::Main,
            ActiveProfile::Auto,
        ]
        .into_iter()
        .enumerate()
        {
            *active_profile
                .write()
                .expect("active profile lock poisoned") = profile;
            let (pause_tx, mut pause_rx) = oneshot::channel();
            let pause_id = format!("agent-thread:tool_{index}");
            pending_pauses
                .lock()
                .expect("pending pause mutex poisoned")
                .insert(pause_id.clone(), PendingToolPause::Permission(pause_tx));
            engine_tx
                .send(EngineToRuntimeEvent::ToolPauseRequested(Box::new(
                    ToolPauseRequest {
                        tool_use_id: pause_id,
                        preview_tool_use_id: Some(format!("tool_{index}")),
                        tool_name: "bash".to_string(),
                        permission_source: None,
                        source_session_id: Some("agent-thread".to_string()),
                        source_agent_label: Some("general".to_string()),
                        kind: ToolPauseKind::Permission(PermissionPreview::Custom {
                            tool_name: "bash".to_string(),
                            payload: serde_json::Map::new(),
                        }),
                    },
                )))
                .await
                .unwrap();

            if profile == ActiveProfile::Auto {
                assert!(matches!(
                    pause_rx.await.unwrap(),
                    ToolPauseResponse::Permission { approved: true, .. }
                ));
                assert!(event_rx.try_recv().is_err());
            } else {
                assert!(matches!(
                    event_rx.recv().await,
                    Some(RuntimeToServerEvent::ToolPauseRequested(_))
                ));
                *active_profile
                    .write()
                    .expect("active profile lock poisoned") = ActiveProfile::Auto;
                assert!(matches!(
                    pause_rx.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
                drop(pause_rx);
            }
        }
        drop(engine_tx);
        assert!(bridge.await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn compact_usage_updates_agent_and_owner_totals_without_ui_compact_events() {
        let owner_usage = SessionUsageSnapshot {
            current_context_tokens: 77,
            total_tokens: 100,
            total_cached_tokens: 10,
            context_window: Some(1_000),
        };
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (persistence_tx, mut persistence_rx) = mpsc::channel(8);
        let (completion_tx, _completion_rx) = mpsc::unbounded_channel();
        let supervisor = AgentTaskSupervisor::new(
            event_tx,
            persistence_tx,
            completion_tx,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(PermissionEngine::empty("/tmp")),
            Arc::new(RwLock::new(ActiveProfile::Main)),
            Arc::new(Mutex::new(owner_usage)),
            Vec::new(),
        );
        let (engine_tx, engine_rx) = mpsc::channel(4);
        let info = task_info(1);
        let bridge = {
            let supervisor = Arc::clone(&supervisor);
            let info = info.clone();
            tokio::spawn(async move {
                supervisor
                    .bridge_engine_events(engine_rx, &info, "openai/test")
                    .await
            })
        };
        let usage = Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            cached_tokens: 1,
        };
        engine_tx
            .send(EngineToRuntimeEvent::CompactSummaryUsageRecorded(usage))
            .await
            .unwrap();
        drop(engine_tx);
        assert!(bridge.await.unwrap().is_empty());

        assert!(matches!(
            persistence_rx.recv().await,
            Some(RuntimePersistenceEvent::RecordThreadTotalUsage { thread_id, usage: saved })
                if thread_id == "thread_1" && saved == usage
        ));
        assert!(matches!(
            persistence_rx.recv().await,
            Some(RuntimePersistenceEvent::RecordOwnerAgentUsage { thread_id, usage: saved })
                if thread_id == "owner" && saved == usage
        ));
        assert!(matches!(
            event_rx.recv().await,
            Some(RuntimeToServerEvent::UsageTotalsChanged {
                total_tokens: 106,
                total_cached_tokens: 11,
            })
        ));
    }

    #[tokio::test]
    async fn depth_one_and_two_bridge_preserve_stream_event_order() {
        for depth in [1, 2] {
            let (event_tx, mut event_rx) = mpsc::channel(16);
            let (persistence_tx, mut persistence_rx) = mpsc::channel(4);
            let (completion_tx, _completion_rx) = mpsc::unbounded_channel();
            let pending_pauses: PendingToolPauses = Arc::new(Mutex::new(HashMap::new()));
            let supervisor = AgentTaskSupervisor::new(
                event_tx,
                persistence_tx,
                completion_tx,
                pending_pauses,
                Arc::new(PermissionEngine::empty("/tmp")),
                Arc::new(RwLock::new(ActiveProfile::Main)),
                Arc::new(Mutex::new(SessionUsageSnapshot::default())),
                Vec::new(),
            );
            let persistence = tokio::spawn(async move {
                while let Some(event) = persistence_rx.recv().await {
                    if let RuntimePersistenceEvent::PersistAgentMessage { ack, .. } = event {
                        let _ = ack.send(Ok(()));
                    }
                }
            });
            let (engine_tx, engine_rx) = mpsc::channel(16);
            let info = task_info(depth);
            let bridge = {
                let supervisor = Arc::clone(&supervisor);
                let info = info.clone();
                tokio::spawn(async move {
                    supervisor
                        .bridge_engine_events(engine_rx, &info, "openai/test")
                        .await
                })
            };
            engine_tx
                .send(EngineToRuntimeEvent::TurnStarted)
                .await
                .unwrap();
            engine_tx
                .send(EngineToRuntimeEvent::ThinkingDelta("think".to_string()))
                .await
                .unwrap();
            engine_tx
                .send(EngineToRuntimeEvent::TextDelta("answer".to_string()))
                .await
                .unwrap();
            engine_tx
                .send(EngineToRuntimeEvent::ToolUse(ToolUseBlock {
                    id: "tool_1".to_string(),
                    name: "read".to_string(),
                    input: HashMap::new(),
                }))
                .await
                .unwrap();
            engine_tx
                .send(EngineToRuntimeEvent::ToolResult(ToolResultBlock {
                    tool_use_id: "tool_1".to_string(),
                    is_error: false,
                    content: "done".to_string(),
                    metadata: None,
                }))
                .await
                .unwrap();
            engine_tx
                .send(EngineToRuntimeEvent::MessageProduced(Message::new(
                    Role::Assistant,
                    vec![ContentBlock::from_text("answer".to_string())],
                )))
                .await
                .unwrap();
            engine_tx
                .send(EngineToRuntimeEvent::TurnEnded)
                .await
                .unwrap();
            drop(engine_tx);

            assert!(bridge.await.unwrap().is_empty());
            drop(supervisor);
            persistence.await.unwrap();
            let mut payloads = Vec::new();
            while let Ok(RuntimeToServerEvent::AgentTaskEvent(event)) = event_rx.try_recv() {
                payloads.push(event.payload);
            }
            assert!(matches!(payloads[0], AgentTaskEvent::TurnStarted));
            assert!(matches!(payloads[1], AgentTaskEvent::ThinkingDelta { .. }));
            assert!(matches!(payloads[2], AgentTaskEvent::TextDelta { .. }));
            assert!(matches!(payloads[3], AgentTaskEvent::ToolUse { .. }));
            assert!(matches!(payloads[4], AgentTaskEvent::ToolResult { .. }));
            assert!(matches!(
                payloads[5],
                AgentTaskEvent::MessageCommitted { .. }
            ));
            assert!(matches!(payloads[6], AgentTaskEvent::TurnEnded));
        }
    }
}
