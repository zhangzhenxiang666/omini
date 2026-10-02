use crate::agent::{AgentSpec, AgentTaskRequest};
use crate::engine::{PendingUserMessage, QueryContext, QueryEngine, SharedPendingUserMessages};
use crate::execution::handle::OutputHandle;
use crate::execution::host::{AgentHost, AgentSessionModel, AgentSessionRequest};
use crate::skills::SkillSummary;
use crate::tasks::{
    BackgroundTaskReservation, DEFAULT_MAX_BACKGROUND_TASKS, TaskCancellation, TaskManager,
};
use crate::tools::{
    PendingToolPauses, ToolExecutionContext, ToolRegistry, ToolResult, ToolRuntimeContext,
    create_agent_registry_from_parent,
};
use jiff::Timestamp;
use omini_config::project::ThreadDir;
use omini_config::{ModelSelection, Settings};
use omini_domain::conversation::{AgentMessage, UserInput};
use omini_domain::input::{InputPart, UserInputIntent};
use omini_domain::task::{TaskCompletion, TaskInfo, TaskKind, TaskStatus};
use omini_model::message::{ContentBlock, Message, Role};
use omini_permissions::PermissionEngine;
use omini_provider_api::{FinishReason, LlmClient};
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::thread_domain::{
    ActiveProfile, AgentTaskEvent, AgentTaskEventEnvelope, AgentTaskExecutionMode, AgentTaskInfo,
    AgentTaskResult, MAX_AGENT_DEPTH, ThreadUsageSnapshot,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{Notify, mpsc};
use tracing::Instrument;
use uuid::Uuid;

const BACKGROUND_TASK_MEMORY_LIMIT: usize = 30;
const MAX_SYNCHRONOUS_AGENT_TASKS: usize = 10;

/// 复用任务完成通知报告未注入消息，避免额外的主会话历史类型。
fn delivery_summary(result: &AgentTaskResult) -> Option<String> {
    result
        .undelivered_messages
        .filter(|count| *count > 0)
        .map(|count| format!("{count} 条消息未进入子 Agent 模型上下文"))
}

fn task_info_from_agent(task: &AgentTaskInfo) -> TaskInfo {
    TaskInfo {
        task_id: task.task_id.clone(),
        owner_thread_id: task.owner_thread_id.clone(),
        kind: TaskKind::SubAgent,
        title: task.title.clone(),
        status: task.status,
        created_at: task.created_at,
        updated_at: task.updated_at,
        completed_at: task.completed_at,
        result_summary: task.result.as_ref().and_then(|result| {
            result
                .output
                .clone()
                .or_else(|| result.error.clone())
                .or_else(|| delivery_summary(result))
        }),
    }
}

#[derive(Debug, Default)]
struct ActiveTaskSlots {
    synchronous: usize,
}

impl ActiveTaskSlots {
    fn reserve_synchronous(&mut self) -> Result<(), String> {
        if self.synchronous >= MAX_SYNCHRONOUS_AGENT_TASKS {
            return Err(format!(
                "synchronous agent task limit reached: at most {MAX_SYNCHRONOUS_AGENT_TASKS} tasks may run concurrently; wait for a task to finish before calling run_agent again"
            ));
        }
        self.synchronous += 1;
        Ok(())
    }

    fn release_synchronous(&mut self) {
        self.synchronous = self
            .synchronous
            .checked_sub(1)
            .expect("releasing unreserved synchronous task slot");
    }
}

struct TaskSlotReservation {
    supervisor: Option<Arc<AgentTaskSupervisor>>,
    _background: Option<BackgroundTaskReservation>,
}

impl Drop for TaskSlotReservation {
    fn drop(&mut self) {
        if let Some(supervisor) = &self.supervisor {
            supervisor.release_synchronous_slot();
        }
    }
}

struct TaskEntry {
    info: AgentTaskInfo,
    accepting_messages: bool,
    sent_messages: HashMap<(String, String), String>,
    cancelled: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    inbox: SharedPendingUserMessages,
}

struct PreparedTask {
    info: AgentTaskInfo,
    settings: Arc<Settings>,
    tool_registry: Arc<ToolRegistry>,
    thread_dir: ThreadDir,
    project: omini_config::project::ProjectDir,
    agent_registry: Arc<crate::agent::AgentRegistry>,
    skill_registry: Arc<crate::skills::SkillRegistry>,
    initial_message: Message,
    llm_context_version: i64,
    warnings: Vec<String>,
    cancelled: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    inbox: SharedPendingUserMessages,
    slot: TaskSlotReservation,
}

/// 归属于主线程的长期服务，管理后台根 task 及其同步后代。
///
/// 子会话与存储资源经 [`AgentHost`] 申请；领域事件经实例输出通道发布。
pub struct AgentTaskSupervisor {
    output: OutputHandle,
    host: Arc<dyn AgentHost>,
    pending_tool_pauses: PendingToolPauses,
    permission_engine: Arc<PermissionEngine>,
    active_profile: Arc<RwLock<ActiveProfile>>,
    owner_usage: Arc<Mutex<ThreadUsageSnapshot>>,
    tasks: Mutex<HashMap<String, TaskEntry>>,
    parent_inbox: Mutex<Option<SharedPendingUserMessages>>,
    active_task_slots: Mutex<ActiveTaskSlots>,
    idle_notify: Notify,
    task_manager: Arc<TaskManager>,
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

#[bon::bon]
impl AgentTaskSupervisor {
    /// 构造任务监督器并返回共享句柄。经 `AgentTaskSupervisor::builder()`
    /// 以命名设置器装配；`initial_tasks` / `background_tasks` 为启动恢复的
    /// 既有任务快照，全新会话可传空集合。装配顺序保持：先建 TaskManager
    /// 与任务表，再结算未投递的完成通知。
    #[builder]
    pub fn new(
        output: OutputHandle,
        host: Arc<dyn AgentHost>,
        completion_tx: mpsc::UnboundedSender<TaskCompletion>,
        pending_tool_pauses: PendingToolPauses,
        permission_engine: Arc<PermissionEngine>,
        active_profile: Arc<RwLock<ActiveProfile>>,
        owner_usage: Arc<Mutex<ThreadUsageSnapshot>>,
        initial_tasks: Vec<AgentTaskInfo>,
        background_tasks: Vec<TaskInfo>,
    ) -> Arc<Self> {
        let mut generic_initial = background_tasks
            .into_iter()
            .map(|task| (task.task_id.clone(), task))
            .collect::<HashMap<_, _>>();
        for task in initial_tasks
            .iter()
            .filter(|task| task.execution_mode == AgentTaskExecutionMode::Background)
        {
            generic_initial.insert(task.task_id.clone(), task_info_from_agent(task));
        }
        let task_manager = TaskManager::new(
            output.clone(),
            Arc::clone(&host),
            generic_initial.into_values().collect(),
            DEFAULT_MAX_BACKGROUND_TASKS,
            completion_tx.clone(),
        );
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
                        accepting_messages: false,
                        sent_messages: HashMap::new(),
                        cancelled: Arc::new(AtomicBool::new(false)),
                        cancel_notify: Arc::new(Notify::new()),
                        inbox: Arc::new(Mutex::new(std::collections::VecDeque::new())),
                    },
                )
            })
            .collect();
        let supervisor = Arc::new(Self {
            output,
            host,
            pending_tool_pauses,
            permission_engine,
            active_profile,
            owner_usage,
            tasks: Mutex::new(tasks),
            parent_inbox: Mutex::new(None),
            active_task_slots: Mutex::new(ActiveTaskSlots::default()),
            idle_notify: Notify::new(),
            task_manager,
        });
        for task in initial_tasks.into_iter().filter(|task| {
            task.parent_task_id.is_none()
                && task.execution_mode == AgentTaskExecutionMode::Background
                && task.status.is_terminal()
                && !task.notification_delivered
        }) {
            supervisor.task_manager.notify_completed(TaskCompletion {
                task_id: task.task_id,
                kind: TaskKind::SubAgent,
                label: task.agent,
                title: task.title,
                status: task.status,
                summary: task.result.as_ref().and_then(delivery_summary),
            });
        }
        supervisor
    }

    pub fn task_manager(&self) -> Arc<TaskManager> {
        Arc::clone(&self.task_manager)
    }

    pub fn set_parent_inbox(&self, inbox: SharedPendingUserMessages) {
        *self
            .parent_inbox
            .lock()
            .expect("agent parent inbox mutex poisoned") = Some(inbox);
    }

    pub async fn send_message(
        &self,
        sender_task_id: Option<&str>,
        sender_depth: u8,
        sender_run_id: Option<&str>,
        tool_use_id: &str,
        target: &str,
        text: &str,
    ) -> Result<(), String> {
        if target == "parent" {
            if sender_depth != 1 || sender_task_id.is_none() {
                return Err("only a first-level child agent can message its parent".to_string());
            }
            let tasks = self.tasks.lock().expect("agent task mutex poisoned");
            let sender = tasks
                .get(sender_task_id.expect("checked above"))
                .ok_or_else(|| "sending child agent was not found".to_string())?;
            if sender.info.depth != 1 || sender.info.parent_task_id.is_some() {
                return Err("messages are only available to direct child agents".to_string());
            }
            if sender.info.status.is_terminal() {
                return Err("completed child agents cannot send messages".to_string());
            }
            let inbox = self
                .parent_inbox
                .lock()
                .expect("agent parent inbox mutex poisoned")
                .clone()
                .ok_or_else(|| "parent agent inbox is unavailable".to_string())?;
            inbox
                .lock()
                .expect("agent parent inbox queue poisoned")
                .push_back(PendingUserMessage::Plain(Message::from_user_text(
                    text.to_string(),
                )));
            return Ok(());
        }

        if sender_depth != 0 || sender_task_id.is_some() {
            return Err("only the main agent can message a child agent".to_string());
        }
        let sender_run_id =
            sender_run_id.ok_or_else(|| "main agent run is unavailable".to_string())?;
        let source = AgentMessage {
            source_run_id: sender_run_id.to_string(),
            tool_use_id: tool_use_id.to_string(),
            text: text.to_string(),
        };
        let source_key = (source.source_run_id.clone(), source.tool_use_id.clone());
        let info = {
            let tasks = self.tasks.lock().expect("agent task mutex poisoned");
            let task = tasks
                .get(target)
                .ok_or_else(|| "child agent task was not found".to_string())?;
            if task.info.depth != 1 || task.info.parent_task_id.is_some() {
                return Err("messages are only available to direct child agents".to_string());
            }
            if !task.accepting_messages || task.info.status != TaskStatus::Running {
                return Err("messages cannot be sent to a completed child agent".to_string());
            }
            task.info.clone()
        };
        self.host
            .enqueue_agent_message(
                &info.task_id,
                &info.owner_thread_id,
                &info.thread_id,
                &source,
            )
            .await
            .map_err(|_| "agent message persistence failed".to_string())?;
        let accepted = {
            let mut tasks = self.tasks.lock().expect("agent task mutex poisoned");
            if let Some(task) = tasks.get_mut(target) {
                if !task.accepting_messages || task.info.status != TaskStatus::Running {
                    false
                } else if let Some(previous) = task.sent_messages.get(&source_key) {
                    return if previous == text {
                        Ok(())
                    } else {
                        Err("agent message source key was reused with different content"
                            .to_string())
                    };
                } else {
                    task.sent_messages.insert(source_key, text.to_string());
                    task.inbox
                        .lock()
                        .expect("agent child inbox queue poisoned")
                        .push_back(PendingUserMessage::MainAgent {
                            model_message: Message::from_user_text(format!(
                                "来自主 Agent 的消息：\n{text}"
                            )),
                            source: source.clone(),
                        });
                    true
                }
            } else {
                false
            }
        };
        if !accepted {
            self.fail_task_messages(target, "子任务在消息入队前结束")
                .await?;
            return Err("child agent finished before message was queued".to_string());
        }
        Ok(())
    }

    pub async fn intervene_agent_run(
        &self,
        run_id: &str,
        message: Message,
        client_source: Option<omini_runtime_contract::thread_domain::ClientMessage>,
    ) -> Result<(), String> {
        let from_client = client_source.is_some();
        let result = self.queue_agent_input(run_id, message, client_source);
        if result.is_err() && from_client {
            self.fail_task_messages(run_id, "子任务已结束，客户端输入未能注入")
                .await?;
        }
        result
    }

    /// 任务锁内完成终态检查和入队，避免收尾边界漏掉已接受的消息。
    fn queue_agent_input(
        &self,
        run_id: &str,
        message: Message,
        client_source: Option<omini_runtime_contract::thread_domain::ClientMessage>,
    ) -> Result<(), String> {
        let tasks = self.tasks.lock().expect("agent task mutex poisoned");
        let task = tasks
            .get(run_id)
            .ok_or_else(|| "agent run was not found".to_string())?;
        if task.info.depth != 1 || task.info.parent_task_id.is_some() {
            return Err("user intervention is only available for direct child agents".to_string());
        }
        if !task.accepting_messages || task.info.status.is_terminal() {
            return Err("user intervention is unavailable for a completed child agent".to_string());
        }
        task.inbox
            .lock()
            .expect("agent child inbox queue poisoned")
            .push_back(match client_source {
                Some(source) => PendingUserMessage::Client {
                    model_message: message,
                    source,
                },
                None => PendingUserMessage::Plain(message),
            });
        Ok(())
    }

    /// 将已接受但未注入的子任务消息结算为失败，防止任务终止时静默丢弃。
    async fn fail_task_messages(&self, task_id: &str, reason: &str) -> Result<u32, String> {
        self.host
            .fail_pending_task_messages(task_id, reason)
            .await
            .map_err(|error| error.to_string())
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

    pub fn read_task(&self, task_id: &str) -> ToolResult {
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
                    task.info.status = TaskStatus::Cancelling;
                    task.accepting_messages = false;
                    task.info.updated_at = Timestamp::now();
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
        for task_id in &cancelling_ids {
            let task = self
                .tasks
                .lock()
                .expect("agent task mutex poisoned")
                .get(task_id)
                .map(|task| task_info_from_agent(&task.info));
            if let Some(task) = task {
                let _ = self.task_manager.update(task).await;
            }
        }
        self.pending_tool_pauses
            .lock()
            .expect("pending tool pause mutex poisoned")
            .retain(|pause_id, _| {
                !cancelling_thread_ids
                    .iter()
                    .any(|thread_id| pause_id.starts_with(&format!("{thread_id}:")))
            });
        if let Err(error) = self.host.set_agent_tasks_cancelling(&cancelling_ids).await {
            tracing::warn!(error = %error, "failed to persist cancelling agent tasks");
        }
        ToolResult::ok(response)
    }

    pub async fn cancel_all(&self) {
        self.task_manager.cancel_all();
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
        self.task_manager.has_active_tasks() || self.has_running_agents()
    }

    fn has_running_agents(&self) -> bool {
        self.tasks
            .lock()
            .expect("agent task mutex poisoned")
            .values()
            .any(|entry| !entry.info.status.is_terminal())
    }

    pub fn mark_notifications_delivered(&self, task_ids: &[String]) {
        self.task_manager.mark_notifications_delivered(task_ids);
        let mut tasks = self.tasks.lock().expect("agent task mutex poisoned");
        for task_id in task_ids {
            if let Some(task) = tasks.get_mut(task_id) {
                task.info.notification_delivered = true;
            }
        }
        prune_delivered_background_history(&mut tasks);
    }

    pub async fn wait_until_idle(&self) {
        loop {
            let notified = self.idle_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.has_running_agents() {
                self.task_manager.wait_until_idle().await;
                return;
            }
            notified.await;
        }
    }

    fn reserve_task_slot(
        self: &Arc<Self>,
        execution_mode: AgentTaskExecutionMode,
    ) -> Result<TaskSlotReservation, String> {
        match execution_mode {
            AgentTaskExecutionMode::Background => Ok(TaskSlotReservation {
                supervisor: None,
                _background: Some(self.task_manager.reserve_background()?),
            }),
            AgentTaskExecutionMode::Synchronous => {
                self.active_task_slots
                    .lock()
                    .expect("agent task slot mutex poisoned")
                    .reserve_synchronous()?;
                Ok(TaskSlotReservation {
                    supervisor: Some(Arc::clone(self)),
                    _background: None,
                })
            }
        }
    }

    fn release_synchronous_slot(&self) {
        self.active_task_slots
            .lock()
            .expect("agent task slot mutex poisoned")
            .release_synchronous();
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
        ));
        let settings = Arc::new(settings);
        let slot = self.reserve_task_slot(execution_mode)?;

        let task_id = Uuid::new_v4().to_string();
        let now = Timestamp::now();
        let initial_context_version = 1;
        let initial_prompt = UserInput {
            intent: UserInputIntent::Message,
            parts: vec![InputPart::Text {
                text: request.prompt.clone(),
            }],
            attachments: Vec::new(),
        };
        let initial_message = Message::from_user_text(request.prompt.clone());
        let model = settings.active_model();
        // 子会话（子线程、任务、初始历史）由宿主原子建立；失败即未生效，
        // core 不再自建 thread 目录或组装线程行，也就没有本地清理路径。
        let session = self
            .host
            .create_agent_session(AgentSessionRequest {
                task_id: task_id.clone(),
                parent_run_id: runtime.run_id.clone().or_else(|| runtime.task_id.clone()),
                parent_task_id: runtime.task_id.clone(),
                owner_thread_id: runtime.owner_thread_id.clone(),
                parent_thread_id: runtime.thread_id.clone(),
                spawn_tool_use_id: ctx.tool_use_id.clone(),
                agent: spec.name.clone(),
                title: request.title.clone(),
                depth,
                execution_mode,
                initial_prompt: initial_prompt.clone(),
                initial_message: initial_message.clone(),
                model: AgentSessionModel {
                    provider: model.provider_id.clone(),
                    model: model.model_id.clone(),
                    thinking_effort: model.thinking_effort.map(|effort| effort.to_string()),
                },
            })
            .await
            .map_err(|error| format!("failed to create agent session: {error}"))?;
        let thread_id = session.thread_id;
        let thread_dir = session.thread_dir;
        let info = AgentTaskInfo {
            task_id: task_id.clone(),
            thread_id: thread_id.clone(),
            parent_run_id: runtime.run_id.clone().or_else(|| runtime.task_id.clone()),
            parent_task_id: runtime.task_id.clone(),
            owner_thread_id: runtime.owner_thread_id.clone(),
            parent_thread_id: runtime.thread_id.clone(),
            spawn_tool_use_id: ctx.tool_use_id.clone(),
            agent: spec.name.clone(),
            title: request.title,
            depth,
            execution_mode,
            status: TaskStatus::Running,
            result: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
            notification_delivered: false,
        };

        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_notify = Arc::new(Notify::new());
        let inbox = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        self.tasks
            .lock()
            .expect("agent task mutex poisoned")
            .insert(
                task_id,
                TaskEntry {
                    info: info.clone(),
                    accepting_messages: true,
                    sent_messages: HashMap::new(),
                    cancelled: Arc::clone(&cancelled),
                    cancel_notify: Arc::clone(&cancel_notify),
                    inbox: Arc::clone(&inbox),
                },
            );
        if execution_mode == AgentTaskExecutionMode::Background {
            let _ = self
                .task_manager
                .register(
                    task_info_from_agent(&info),
                    Some(TaskCancellation::new(
                        Arc::clone(&cancelled),
                        Arc::clone(&cancel_notify),
                    )),
                )
                .await;
        }
        self.emit(
            &info,
            AgentTaskEvent::Started {
                parent_thread_id: info.parent_thread_id.clone(),
                spawn_tool_use_id: info.spawn_tool_use_id.clone(),
                agent: info.agent.clone(),
                title: info.title.clone(),
                initial_prompt,
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
            inbox,
            slot,
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
            inbox,
            slot: _slot,
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
        let model = settings.active_model();
        let llm_client = LlmClient::new(
            model.protocol,
            model
                .api_key
                .as_ref()
                .map(|secret| secret.expose().to_string())
                .unwrap_or_default(),
            model.base_url.clone(),
        );
        let runtime = Arc::new(ToolRuntimeContext {
            thread_id: info.thread_id.clone(),
            run_id: Some(info.task_id.clone()),
            thread_type: "agent".to_string(),
            agent_label: Some(info.agent.clone()),
            thread_dir,
            llm_context_version: Arc::new(std::sync::atomic::AtomicI64::new(llm_context_version)),
            agent_depth: info.depth,
            task_id: Some(info.task_id.clone()),
            owner_thread_id: info.owner_thread_id.clone(),
            agent_registry,
            skill_registry,
            task_manager: Some(self.task_manager()),
            task_supervisor: Some(Arc::clone(&self)),
            project,
        });
        let mut messages = vec![initial_message];
        let (child_tx, child_rx) = mpsc::channel(256);
        // 子任务与主 Run 共用同一执行事件接收端（子任务投影）。
        let model_ref = format!("{}/{}", model.provider_id, model.model_id);
        let bridge = crate::runtime::event_sink::spawn_child_sink(
            crate::runtime::event_sink::ChildSinkConfig {
                host: Arc::clone(&self.host),
                output: self.output.clone(),
                active_profile_handle: Arc::clone(&self.active_profile),
                pending_tool_pauses: Arc::clone(&self.pending_tool_pauses),
                info: info.clone(),
                model_ref,
                owner_usage: Arc::clone(&self.owner_usage),
            },
            child_rx,
        );
        let engine = QueryEngine::with_shared_user_messages(
            Arc::clone(&self.pending_tool_pauses),
            Arc::clone(&self.permission_engine),
            Arc::clone(&cancel_notify),
            inbox,
        );
        let result = loop {
            let result = engine
                .run_query(
                    QueryContext {
                        messages: &mut messages,
                        settings: Arc::clone(&settings),
                        llm_client: llm_client.clone(),
                        tool_registry: Arc::clone(&tool_registry),
                        active_profile: Arc::clone(&self.active_profile),
                        runtime_context: Some(Arc::clone(&runtime)),
                        requires_internal_input: false,
                    },
                    child_tx.clone(),
                    Arc::clone(&cancelled),
                )
                .instrument(task_span.clone())
                .await;
            let continue_for_message = {
                let mut tasks = self.tasks.lock().expect("agent task mutex poisoned");
                let task = tasks
                    .get_mut(&info.task_id)
                    .expect("running agent task must exist");
                let pending = !task
                    .inbox
                    .lock()
                    .expect("agent child inbox queue poisoned")
                    .is_empty();
                if cancelled.load(Ordering::Relaxed)
                    || matches!(result.finish_reason, FinishReason::Error(_))
                    || !pending
                {
                    task.accepting_messages = false;
                    false
                } else {
                    true
                }
            };
            if !continue_for_message {
                break result;
            }
        };
        drop(child_tx);
        match bridge.await {
            Ok(bridge_warnings) => warnings.extend(bridge_warnings),
            Err(error) => warnings.push(format!("agent event bridge failed: {error}")),
        }
        let status = if cancelled.load(Ordering::Relaxed) {
            TaskStatus::Cancelled
        } else if matches!(result.finish_reason, FinishReason::Error(_)) {
            TaskStatus::Failed
        } else {
            TaskStatus::Completed
        };
        let task_result = AgentTaskResult {
            output: extract_final_text(&messages),
            error: match result.finish_reason {
                FinishReason::Error(error) => Some(error),
                _ => None,
            },
            warnings,
            undelivered_messages: None,
        };
        self.finish_task(&info.task_id, status, task_result).await
    }

    async fn finish_task(
        &self,
        task_id: &str,
        mut status: TaskStatus,
        mut result: AgentTaskResult,
    ) -> AgentTaskInfo {
        if let Some(task) = self
            .tasks
            .lock()
            .expect("agent task mutex poisoned")
            .get_mut(task_id)
        {
            task.accepting_messages = false;
        }
        match self
            .fail_task_messages(task_id, "子任务结束前未能注入消息")
            .await
        {
            Ok(0) => {}
            Ok(count) => {
                result.undelivered_messages = Some(count);
                if status == TaskStatus::Completed {
                    status = TaskStatus::Failed;
                }
            }
            Err(error) => {
                tracing::error!(task_id, %error, "failed to settle pending agent messages");
                result
                    .warnings
                    .push(format!("消息投递状态未能结算：{error}"));
                result.error.get_or_insert(error);
                status = TaskStatus::Failed;
            }
        }
        let completed_at = Timestamp::now();
        let persistence_result = self
            .host
            .finish_agent_task(task_id, status, &result, completed_at)
            .await
            .map_err(|error| error.to_string());
        let (info, notify_owner) = {
            let mut tasks = self.tasks.lock().expect("agent task mutex poisoned");
            let entry = tasks
                .get_mut(task_id)
                .expect("finishing agent task must exist");
            let final_status = if persistence_result.is_ok() {
                status
            } else {
                TaskStatus::Failed
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
        if info.execution_mode == AgentTaskExecutionMode::Background {
            let _ = self.task_manager.update(task_info_from_agent(&info)).await;
        }
        self.emit(
            &info,
            AgentTaskEvent::Finished {
                status: (info.execution_mode == AgentTaskExecutionMode::Synchronous)
                    .then_some(info.status),
                result: info.result.clone(),
            },
        )
        .await;
        if notify_owner {
            self.task_manager.notify_completed(TaskCompletion {
                task_id: info.task_id.clone(),
                kind: TaskKind::SubAgent,
                label: info.agent.clone(),
                title: info.title.clone(),
                status: info.status,
                summary: info.result.as_ref().and_then(delivery_summary),
            });
        }
        self.idle_notify.notify_waiters();
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
                    TaskStatus::Failed,
                    AgentTaskResult {
                        output: None,
                        error: Some(error),
                        warnings: Vec::new(),
                        undelivered_messages: None,
                    },
                )
                .await,
            );
        }
        None
    }

    async fn emit(&self, info: &AgentTaskInfo, payload: AgentTaskEvent) {
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
struct TaskStatusResponse<'a> {
    task_id: &'a str,
    status: TaskStatus,
}

#[derive(Serialize)]
struct AgentTaskResultResponse<'a> {
    task_id: &'a str,
    status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a AgentTaskResult>,
}

fn task_status_payload(info: &AgentTaskInfo) -> String {
    serde_json::to_string(&TaskStatusResponse {
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
    if info.status == TaskStatus::Completed {
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
    let parent_model = parent_settings.active_model();
    if let Err(error) = settings.select_model(ModelSelection {
        active_provider: model_spec.provider.clone(),
        model: model_spec.model.clone(),
        thinking_effort: None,
    }) {
        warnings.push(format!(
            "{}; falling back to {}/{}",
            error, parent_model.provider_id, parent_model.model_id
        ));
    }
    (settings, warnings)
}

fn agent_system_prompt(parent: &Settings, spec: &AgentSpec, skills: &[SkillSummary]) -> String {
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
    prompt.push_str("Return a concise final result for the parent agent.\n\n");
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
    use crate::execution::handle::OutputHandle;
    use crate::runtime::event_sink;
    use crate::test_support::RecordingHost;
    use crate::tools::read_task_tool::{ReadTaskInput, ReadTaskTool};
    use crate::tools::{PendingToolPause, PendingToolPauses, Tool};
    use crate::types::events::EngineToRuntimeEvent;
    use jiff::SignedDuration;
    use omini_domain::usage::Usage;
    use omini_model::message::{ToolResultBlock, ToolUseBlock};
    use omini_runtime_contract::thread_domain::{
        PermissionPreview, ThreadUsageSnapshot, ToolPauseKind, ToolPauseRequest, ToolPauseResponse,
    };
    use std::collections::HashMap;
    use tokio::sync::oneshot;

    use super::*;

    fn task_info(depth: u8) -> AgentTaskInfo {
        let now = Timestamp::now();
        AgentTaskInfo {
            task_id: format!("task_{depth}"),
            thread_id: format!("thread_{depth}"),
            parent_run_id: (depth == 1).then(|| "root_run".to_string()),
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
            status: TaskStatus::Running,
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
        Arc<RecordingHost>,
        PendingToolPauses,
        Arc<RwLock<ActiveProfile>>,
    ) {
        let (output, events) = OutputHandle::new(32);
        let host = Arc::new(RecordingHost::default());
        let (completion_tx, _completion_rx) = mpsc::unbounded_channel();
        let pending_pauses: PendingToolPauses = Arc::new(Mutex::new(HashMap::new()));
        let active_profile = Arc::new(RwLock::new(ActiveProfile::Main));
        let supervisor = AgentTaskSupervisor::builder()
            .output(output)
            .host(host.clone())
            .completion_tx(completion_tx)
            .pending_tool_pauses(Arc::clone(&pending_pauses))
            .permission_engine(Arc::new(PermissionEngine::empty("/tmp")))
            .active_profile(Arc::clone(&active_profile))
            .owner_usage(Arc::new(Mutex::new(ThreadUsageSnapshot::default())))
            .initial_tasks(initial_tasks)
            .background_tasks(Vec::new())
            .build();
        (
            supervisor,
            events_into_runtime_events(events),
            host,
            pending_pauses,
            active_profile,
        )
    }

    /// 把输出流裁剪成纯事件流，供现有断言复用。
    fn events_into_runtime_events(
        mut events: crate::execution::AgentEvents,
    ) -> mpsc::Receiver<RuntimeToServerEvent> {
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move {
            while let Some(crate::execution::AgentOutput::Event(event)) = events.recv().await {
                let _ = tx.send(*event).await;
            }
        });
        rx
    }

    /// 构造主 Agent 工具上下文，使用真实任务管理器和 Supervisor 的查询路径。
    fn read_context(supervisor: &Arc<AgentTaskSupervisor>) -> ToolExecutionContext {
        let mut context = ToolExecutionContext::test("read_task");
        let project = omini_config::project::ProjectDir::from_path("/tmp".into());
        context.runtime = Some(Arc::new(ToolRuntimeContext {
            thread_id: "owner".into(),
            run_id: None,
            thread_type: "main".into(),
            agent_label: None,
            thread_dir: project.thread("owner"),
            llm_context_version: Arc::new(std::sync::atomic::AtomicI64::new(1)),
            agent_depth: 0,
            task_id: None,
            owner_thread_id: "owner".into(),
            agent_registry: Arc::new(crate::agent::AgentRegistry {
                agents: HashMap::new(),
                diagnostics: Vec::new(),
            }),
            skill_registry: Arc::new(crate::skills::SkillRegistry {
                skills: HashMap::new(),
                diagnostics: Vec::new(),
            }),
            task_manager: Some(supervisor.task_manager()),
            task_supervisor: Some(Arc::clone(supervisor)),
            project,
        }));
        context
    }

    /// 两类任务的读取均立即返回，仅未结束状态附带提示，并保留原有终态结果。

    #[tokio::test]
    async fn reject_uncommitted_message() {
        // 给定子消息持久化失败，当引擎等待提交确认，则返回错误且不发布已提交消息。
        let host = Arc::new(RecordingHost::default());
        host.fail_operation("persist_agent_message", "storage unavailable");
        let (output, mut events) = OutputHandle::new(16);
        let (engine_tx, engine_rx) = mpsc::channel(16);
        let sink = crate::runtime::event_sink::spawn_child_sink(
            crate::runtime::event_sink::ChildSinkConfig {
                host,
                output,
                active_profile_handle: Arc::new(RwLock::new(ActiveProfile::Main)),
                pending_tool_pauses: Arc::new(Mutex::new(HashMap::new())),
                info: task_info(1),
                model_ref: "test/model".into(),
                owner_usage: Arc::new(Mutex::new(ThreadUsageSnapshot::default())),
            },
            engine_rx,
        );
        engine_tx
            .send(EngineToRuntimeEvent::MessageProduced(
                Message::from_user_text("uncommitted".into()),
            ))
            .await
            .unwrap();
        let error = crate::engine::commit_events(&engine_tx).await.unwrap_err();
        assert!(error.contains("storage unavailable"));
        drop(engine_tx);
        assert!(!sink.await.unwrap().is_empty());
        while let Some(output) = events.recv().await {
            if let crate::execution::AgentOutput::Event(event) = output {
                assert!(!matches!(
                    *event,
                    RuntimeToServerEvent::AgentTaskEvent(
                        omini_runtime_contract::thread_domain::AgentTaskEventEnvelope {
                            payload: AgentTaskEvent::MessageCommitted { .. },
                            ..
                        }
                    )
                ));
            }
        }
    }

    #[tokio::test]
    async fn read_task_guidance() {
        for status in [
            TaskStatus::Running,
            TaskStatus::Cancelling,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
            TaskStatus::Interrupted,
        ] {
            let mut agent = task_info(1);
            agent.status = status;
            if status.is_terminal() {
                agent.result = Some(AgentTaskResult {
                    output: Some("agent output".into()),
                    error: (status == TaskStatus::Failed).then(|| "agent failed".into()),
                    warnings: vec!["model fallback".into()],
                    undelivered_messages: None,
                });
            }
            let (supervisor, _events, _host, _pauses, _profile) =
                test_supervisor(vec![agent.clone()]);
            let mut bash = task_info_from_agent(&agent);
            bash.task_id = "bash_1".into();
            bash.kind = TaskKind::Bash;
            bash.result_summary = status.is_terminal().then(|| "bash output".into());
            supervisor
                .task_manager
                .register(bash.clone(), None)
                .await
                .unwrap();

            for (task_id, baseline) in [
                ("task_1", task_result_payload(&agent)),
                ("bash_1", serde_json::to_string(&bash).unwrap()),
            ] {
                let response = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    ReadTaskTool.call(
                        ReadTaskInput {
                            task_id: task_id.into(),
                        },
                        read_context(&supervisor),
                    ),
                )
                .await
                .expect("read_task must not wait for completion");
                assert!(!response.is_error);
                let mut payload: serde_json::Value =
                    serde_json::from_str(&response.output).unwrap();
                let guidance = payload.as_object_mut().unwrap().remove("guidance");
                assert_eq!(guidance.is_some(), !status.is_terminal());
                if let Some(guidance) = guidance {
                    assert!(guidance.as_str().is_some_and(|hint| !hint.is_empty()));
                }
                assert_eq!(
                    payload,
                    serde_json::from_str::<serde_json::Value>(&baseline).unwrap()
                );
            }
            // 同步执行共享的结果序列化保持原样，不包含读取工具特有的提示。
            assert!(!task_result_payload(&agent).contains("guidance"));
        }
    }

    /// 读取提示不能绕过任务归属、主 Agent 权限或未知任务检查。
    #[tokio::test]
    async fn read_task_boundaries() {
        let (supervisor, _events, _host, _pauses, _profile) = test_supervisor(vec![task_info(1)]);
        for (task_id, owner, depth) in [
            ("missing", "owner", 0),
            ("task_1", "other", 0),
            ("task_1", "owner", 1),
        ] {
            let mut context = read_context(&supervisor);
            let runtime = Arc::make_mut(context.runtime.as_mut().unwrap());
            runtime.owner_thread_id = owner.into();
            runtime.agent_depth = depth;
            let response = ReadTaskTool
                .call(
                    ReadTaskInput {
                        task_id: task_id.into(),
                    },
                    context,
                )
                .await;
            assert!(response.is_error);
            assert!(!response.output.contains("guidance"));
        }
    }

    #[tokio::test]
    async fn task_slots_enforce_per_mode_limits_and_release_after_drop() {
        let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
            test_supervisor(Vec::new());
        let mut background_slots = Vec::new();
        for _ in 0..DEFAULT_MAX_BACKGROUND_TASKS {
            background_slots.push(
                supervisor
                    .reserve_task_slot(AgentTaskExecutionMode::Background)
                    .expect("background task should reserve a slot"),
            );
        }
        let background_error =
            match supervisor.reserve_task_slot(AgentTaskExecutionMode::Background) {
                Ok(_) => panic!("background task above the limit should be rejected"),
                Err(error) => error,
            };
        assert_eq!(
            background_error,
            "background task limit reached: at most 8 tasks may run concurrently"
        );

        let mut synchronous_slots = Vec::new();
        for _ in 0..MAX_SYNCHRONOUS_AGENT_TASKS {
            synchronous_slots.push(
                supervisor
                    .reserve_task_slot(AgentTaskExecutionMode::Synchronous)
                    .expect("synchronous task should reserve a slot"),
            );
        }
        let synchronous_error =
            match supervisor.reserve_task_slot(AgentTaskExecutionMode::Synchronous) {
                Ok(_) => panic!("synchronous task above the limit should be rejected"),
                Err(error) => error,
            };
        assert_eq!(
            synchronous_error,
            "synchronous agent task limit reached: at most 10 tasks may run concurrently; wait for a task to finish before calling run_agent again"
        );

        drop(background_slots.pop());
        supervisor
            .reserve_task_slot(AgentTaskExecutionMode::Background)
            .expect("released background slot should be reusable");
    }

    #[test]
    fn model_visible_task_payloads_hide_internal_fields_and_empty_warnings() {
        let mut info = task_info(1);
        info.status = TaskStatus::Completed;
        info.result = Some(AgentTaskResult {
            output: Some("done".to_string()),
            error: None,
            warnings: Vec::new(),
            undelivered_messages: None,
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
    async fn finish_delivery_count() {
        // 给定子任务正常结束时仍有一条已入队、未注入的消息。
        let initial = task_info(1);
        let task_id = initial.task_id.clone();
        let (supervisor, _events, host, _pauses, _profile) = test_supervisor(vec![initial]);
        host.set_failed_message_count(1);

        // 当终态提交后，结果必须标记失败并携带未送达数量；
        // 且先结算待投递消息、再写入任务终态。
        let finished = supervisor
            .finish_task(
                &task_id,
                TaskStatus::Completed,
                AgentTaskResult {
                    output: Some("done".into()),
                    error: None,
                    warnings: Vec::new(),
                    undelivered_messages: None,
                },
            )
            .await;
        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.result.unwrap().undelivered_messages, Some(1));
        assert_eq!(host.call_count("fail_pending_task_messages"), 1);
        assert_eq!(host.call_count("finish_agent_task"), 1);
        let calls = host.calls();
        assert!(
            calls.iter().position(|c| c == "fail_pending_task_messages")
                < calls.iter().position(|c| c == "finish_agent_task")
        );
    }

    #[tokio::test]
    async fn terminal_cancel_is_idempotent_and_returns_only_status() {
        let mut info = task_info(1);
        info.status = TaskStatus::Failed;
        let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
            test_supervisor(vec![info]);

        let response = supervisor.cancel_task("task_1").await;

        assert!(!response.is_error);
        let payload: serde_json::Value = serde_json::from_str(&response.output).unwrap();
        assert_eq!(payload["task_id"], "task_1");
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload.as_object().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn recovery_keeps_background_history_but_drops_terminal_synchronous_tasks() {
        let mut background = task_info(1);
        background.status = TaskStatus::Completed;
        background.notification_delivered = true;
        let mut synchronous = task_info(2);
        synchronous.status = TaskStatus::Failed;
        let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
            test_supervisor(vec![background, synchronous]);

        assert!(!supervisor.read_task("task_1").is_error);
        assert!(supervisor.read_task("task_2").is_error);
    }

    #[tokio::test]
    async fn recovery_keeps_recent_delivered_background_history_limit() {
        let now = Timestamp::now();
        let mut tasks = Vec::new();
        for index in 0..35 {
            let mut task = task_info(1);
            task.task_id = format!("done_{index:02}");
            task.thread_id = format!("thread_done_{index:02}");
            task.spawn_tool_use_id = format!("spawn_done_{index:02}");
            task.status = TaskStatus::Completed;
            task.notification_delivered = true;
            task.updated_at = now + SignedDuration::from_secs(i64::from(index));
            task.completed_at = Some(task.updated_at);
            tasks.push(task);
        }

        let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        for index in 0..5 {
            assert!(supervisor.read_task(&format!("done_{index:02}")).is_error);
        }
        for index in 5..35 {
            assert!(!supervisor.read_task(&format!("done_{index:02}")).is_error);
        }
    }

    #[tokio::test]
    async fn recovery_keeps_running_and_undelivered_background_tasks_beyond_limit() {
        let now = Timestamp::now();
        let mut tasks = Vec::new();
        for index in 0..35 {
            let mut task = task_info(1);
            task.task_id = format!("done_{index:02}");
            task.thread_id = format!("thread_done_{index:02}");
            task.spawn_tool_use_id = format!("spawn_done_{index:02}");
            task.status = TaskStatus::Completed;
            task.notification_delivered = true;
            task.updated_at = now + SignedDuration::from_secs(i64::from(index));
            task.completed_at = Some(task.updated_at);
            tasks.push(task);
        }
        let mut running = task_info(1);
        running.task_id = "running_old".to_string();
        running.thread_id = "thread_running_old".to_string();
        running.spawn_tool_use_id = "spawn_running_old".to_string();
        running.updated_at = now - SignedDuration::from_secs(100);
        tasks.push(running);
        let mut undelivered = task_info(1);
        undelivered.task_id = "undelivered_old".to_string();
        undelivered.thread_id = "thread_undelivered_old".to_string();
        undelivered.spawn_tool_use_id = "spawn_undelivered_old".to_string();
        undelivered.status = TaskStatus::Completed;
        undelivered.notification_delivered = false;
        undelivered.updated_at = now - SignedDuration::from_secs(101);
        undelivered.completed_at = Some(undelivered.updated_at);
        tasks.push(undelivered);

        let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        assert!(!supervisor.read_task("running_old").is_error);
        assert!(!supervisor.read_task("undelivered_old").is_error);
        assert!(supervisor.read_task("done_00").is_error);
    }

    #[tokio::test]
    async fn mark_notifications_delivered_prunes_old_delivered_background_tasks() {
        let now = Timestamp::now();
        let mut tasks = Vec::new();
        let mut delivered_ids = Vec::new();
        for index in 0..32 {
            let mut task = task_info(1);
            task.task_id = format!("task_{index:02}");
            task.thread_id = format!("thread_{index:02}");
            task.spawn_tool_use_id = format!("spawn_{index:02}");
            task.status = TaskStatus::Completed;
            task.notification_delivered = false;
            task.updated_at = now + SignedDuration::from_secs(i64::from(index));
            task.completed_at = Some(task.updated_at);
            delivered_ids.push(task.task_id.clone());
            tasks.push(task);
        }

        let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        supervisor.mark_notifications_delivered(&delivered_ids);

        assert!(supervisor.read_task("task_00").is_error);
        assert!(supervisor.read_task("task_01").is_error);
        assert!(!supervisor.read_task("task_02").is_error);
        assert!(!supervisor.read_task("task_31").is_error);
    }

    #[tokio::test]
    async fn mark_notifications_delivered_does_not_prune_running_or_undelivered_tasks() {
        let now = Timestamp::now();
        let mut tasks = Vec::new();
        let mut delivered_ids = Vec::new();
        for index in 0..31 {
            let mut task = task_info(1);
            task.task_id = format!("done_{index:02}");
            task.thread_id = format!("thread_done_{index:02}");
            task.spawn_tool_use_id = format!("spawn_done_{index:02}");
            task.status = TaskStatus::Completed;
            task.notification_delivered = false;
            task.updated_at = now + SignedDuration::from_secs(i64::from(index));
            task.completed_at = Some(task.updated_at);
            delivered_ids.push(task.task_id.clone());
            tasks.push(task);
        }
        let mut running = task_info(1);
        running.task_id = "running_old".to_string();
        running.thread_id = "thread_running_old".to_string();
        running.spawn_tool_use_id = "spawn_running_old".to_string();
        running.updated_at = now - SignedDuration::from_secs(100);
        tasks.push(running);
        let mut undelivered = task_info(1);
        undelivered.task_id = "undelivered_old".to_string();
        undelivered.thread_id = "thread_undelivered_old".to_string();
        undelivered.spawn_tool_use_id = "spawn_undelivered_old".to_string();
        undelivered.status = TaskStatus::Completed;
        undelivered.notification_delivered = false;
        undelivered.updated_at = now - SignedDuration::from_secs(101);
        undelivered.completed_at = Some(undelivered.updated_at);
        tasks.push(undelivered);

        let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
            test_supervisor(tasks);

        supervisor.mark_notifications_delivered(&delivered_ids);

        assert!(!supervisor.read_task("running_old").is_error);
        assert!(!supervisor.read_task("undelivered_old").is_error);
        assert!(supervisor.read_task("done_00").is_error);
    }

    #[tokio::test]
    async fn synchronous_terminal_and_panicked_executions_are_removed() {
        for status in [TaskStatus::Completed, TaskStatus::Failed] {
            let initial = task_info(2);
            let task_id = initial.task_id.clone();
            let (supervisor, _event_rx, _host, _pending_pauses, _active_profile) =
                test_supervisor(vec![initial.clone()]);
            let mut finished = initial;
            finished.status = status;
            finished.result = Some(AgentTaskResult {
                output: (status == TaskStatus::Completed).then(|| "done".to_string()),
                error: (status == TaskStatus::Failed).then(|| "failed".to_string()),
                warnings: Vec::new(),
                undelivered_messages: None,
            });

            let response = supervisor
                .finish_synchronous_execution(&task_id, tokio::spawn(async move { finished }))
                .await;

            assert_eq!(response.is_error, status != TaskStatus::Completed);
            let payload: serde_json::Value = serde_json::from_str(&response.output).unwrap();
            assert_eq!(payload["task_id"], task_id);
            assert_eq!(payload["status"], status.as_str());
            assert!(supervisor.read_task(&task_id).is_error);
        }

        let initial = task_info(2);
        let task_id = initial.task_id.clone();
        let (supervisor, _event_rx, host, _pending_pauses, _active_profile) =
            test_supervisor(vec![initial]);
        let response = supervisor
            .finish_synchronous_execution(
                &task_id,
                tokio::spawn(async move { panic!("synthetic task panic") }),
            )
            .await;

        // 给定 panic 收尾会先结算待投递消息，再写入任务终态。
        assert_eq!(host.call_count("fail_pending_task_messages"), 1);
        assert_eq!(host.call_count("finish_agent_task"), 1);
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
        assert!(supervisor.read_task(&task_id).is_error);
    }

    #[tokio::test]
    async fn child_sink_reads_current_profile_for_each_permission_request() {
        let (_supervisor, _event_rx, host, pending_pauses, active_profile) =
            test_supervisor(Vec::new());
        let (sink_output, sink_events) = OutputHandle::new(8);
        let mut event_rx = events_into_runtime_events(sink_events);
        let (engine_tx, engine_rx) = mpsc::channel(4);
        let info = task_info(1);
        let bridge = event_sink::spawn_child_sink(
            crate::runtime::event_sink::ChildSinkConfig {
                host: host.clone(),
                output: sink_output,
                active_profile_handle: Arc::clone(&active_profile),
                pending_tool_pauses: Arc::clone(&pending_pauses),
                info,
                model_ref: "openai/test".to_string(),
                owner_usage: Arc::new(Mutex::new(ThreadUsageSnapshot::default())),
            },
            engine_rx,
        );

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
                        source_thread_id: Some("agent-thread".to_string()),
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
        let owner_usage = ThreadUsageSnapshot {
            current_context_tokens: 77,
            total_tokens: 100,
            total_cached_tokens: 10,
            context_window: Some(1_000),
        };
        let (_supervisor, _event_rx, host, _pending_pauses, active_profile) =
            test_supervisor(Vec::new());
        let (sink_output, sink_events) = OutputHandle::new(8);
        let mut event_rx = events_into_runtime_events(sink_events);
        let (engine_tx, engine_rx) = mpsc::channel(4);
        let info = task_info(1);
        let bridge = event_sink::spawn_child_sink(
            crate::runtime::event_sink::ChildSinkConfig {
                host: host.clone(),
                output: sink_output,
                active_profile_handle: Arc::clone(&active_profile),
                pending_tool_pauses: Arc::new(Mutex::new(HashMap::new())),
                info,
                model_ref: "openai/test".to_string(),
                owner_usage: Arc::new(Mutex::new(owner_usage)),
            },
            engine_rx,
        );
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

        // 子线程总量与 owner 归属各记录一次，且不产生 compact UI 事件。
        assert_eq!(host.call_count("record_thread_total_usage"), 1);
        assert_eq!(host.call_count("record_owner_agent_usage"), 1);
        assert!(matches!(
            event_rx.recv().await,
            Some(RuntimeToServerEvent::UsageTotalsChanged {
                total_tokens: 106,
                total_cached_tokens: 11,
            })
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn depth_one_and_two_bridge_preserve_stream_event_order() {
        for depth in [1, 2] {
            let (_supervisor, _event_rx, host, pending_pauses, active_profile) =
                test_supervisor(Vec::new());
            let (sink_output, sink_events) = OutputHandle::new(16);
            let mut event_rx = events_into_runtime_events(sink_events);
            let (engine_tx, engine_rx) = mpsc::channel(16);
            let info = task_info(depth);
            let bridge = event_sink::spawn_child_sink(
                crate::runtime::event_sink::ChildSinkConfig {
                    host: host.clone(),
                    output: sink_output,
                    active_profile_handle: Arc::clone(&active_profile),
                    pending_tool_pauses: pending_pauses,
                    info,
                    model_ref: "openai/test".to_string(),
                    owner_usage: Arc::new(Mutex::new(ThreadUsageSnapshot::default())),
                },
                engine_rx,
            );
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
