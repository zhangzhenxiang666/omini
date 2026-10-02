//! Agent 句柄、输出通道与运行预留。
//!
//! `AgentHandle` 可克隆，只发送命令、不持有实例任务所有权；实例生命周期由
//! `AgentInstance`（见 `instance.rs`）负责。`AgentEvents` 是独占的单消费者
//! 输出接收端：有界、不静默丢弃事件；宿主接好消费者后实例才启动，输出断开
//! 会触发实例收尾。

use crate::agent::AgentTaskSupervisor;
use crate::error::CoreError;
use crate::execution::snapshot::{AgentLifecycle, AgentSnapshot, CurrentRun};
use crate::mcp::McpManager;
use crate::runtime::capabilities::CapabilityStore;
use crate::runtime::command::AgentCommand;
use crate::runtime::user_input;
use crate::tools::PendingToolPauses;
use omini_config::Settings;
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::thread as thread_types;
use omini_runtime_contract::thread_domain::ActiveProfile;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// 实例输出：领域事件与生命周期公告。
///
/// `Idle` 表示实例当前无预留、无运行、无未完成任务且不在收尾，宿主可据此
/// 回收；`Closed` 之后输出随即关闭。
#[derive(Debug)]
pub enum AgentOutput {
    /// 领域事件装箱，避免与 `Idle`/`Closed` 公告的尺寸差撑大整个枚举。
    Event(Box<RuntimeToServerEvent>),
    Idle,
    Closed,
}

/// `AgentEvents` 的独占接收端。
pub struct AgentEvents {
    rx: mpsc::Receiver<AgentOutput>,
}

impl AgentEvents {
    pub async fn recv(&mut self) -> Option<AgentOutput> {
        self.rx.recv().await
    }
}

/// 实例内部共享的输出端；发送失败即认为输出断开，触发实例收尾。
#[derive(Clone)]
pub(crate) struct OutputHandle {
    tx: mpsc::Sender<AgentOutput>,
    closed: Arc<AtomicBool>,
}

impl OutputHandle {
    pub(crate) fn new(capacity: usize) -> (Self, AgentEvents) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                closed: Arc::new(AtomicBool::new(false)),
            },
            AgentEvents { rx },
        )
    }

    /// 发送一条输出；返回 `false` 表示消费者已断开。
    pub(crate) async fn send(&self, output: AgentOutput) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return false;
        }
        if self.tx.send(output).await.is_err() {
            self.closed.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }

    pub(crate) async fn send_event(&self, event: RuntimeToServerEvent) -> bool {
        self.send(AgentOutput::Event(Box::new(event))).await
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// 等待输出通道关闭（消费者断开）；用于空闲实例感知回收。
    pub(crate) async fn closed(&self) {
        self.tx.closed().await;
    }
}

/// 生命周期与运行资格共用一把锁，预留、内部运行和关闭原子协调。
pub(crate) struct RunGate {
    state: Mutex<GateState>,
    released: tokio::sync::Notify,
    finished: tokio::sync::watch::Sender<bool>,
    closing: tokio::sync::Notify,
}

struct GateState {
    lifecycle: AgentLifecycle,
    slot: GateSlot,
    deferred: bool,
}

enum GateSlot {
    Free,
    Reserved { run_id: String },
    Active { run_id: String },
    Maintenance,
}

impl Default for RunGate {
    fn default() -> Self {
        Self {
            state: Mutex::new(GateState {
                lifecycle: AgentLifecycle::Active,
                slot: GateSlot::Free,
                deferred: false,
            }),
            released: tokio::sync::Notify::new(),
            finished: tokio::sync::watch::channel(false).0,
            closing: tokio::sync::Notify::new(),
        }
    }
}

impl RunGate {
    pub(crate) fn reserve(&self) -> Option<String> {
        let mut state = self.state.lock().expect("run gate lock poisoned");
        if state.lifecycle != AgentLifecycle::Active || !matches!(state.slot, GateSlot::Free) {
            return None;
        }
        let run_id = Uuid::new_v4().to_string();
        state.slot = GateSlot::Reserved {
            run_id: run_id.clone(),
        };
        Some(run_id)
    }

    /// 释放一个未提交的预留;生产路径由 `RunReservation::drop` 调用,
    /// 亦供装配测试模拟宿主放弃受理窗口。
    pub(crate) fn release(&self, run_id: &str) {
        let mut state = self.state.lock().expect("run gate lock poisoned");
        if matches!(&state.slot, GateSlot::Reserved { run_id: current } if current == run_id) {
            state.slot = GateSlot::Free;
            self.released.notify_one();
        }
    }

    pub(crate) async fn wait_reservation_released(&self) {
        self.released.notified().await;
    }

    pub(crate) fn activate(&self, run_id: &str) -> bool {
        let mut state = self.state.lock().expect("run gate lock poisoned");
        if state.lifecycle == AgentLifecycle::Active
            && matches!(&state.slot, GateSlot::Reserved { run_id: current } if current == run_id)
        {
            state.slot = GateSlot::Active {
                run_id: run_id.to_string(),
            };
            state.deferred = false;
            true
        } else {
            false
        }
    }

    /// 内部通知不得抢占宿主已预留的运行；待处理事实留在实例中。
    pub(crate) fn start_internal(&self, run_id: &str) -> bool {
        let mut state = self.state.lock().expect("run gate lock poisoned");
        if state.lifecycle == AgentLifecycle::Active && matches!(state.slot, GateSlot::Free) {
            state.slot = GateSlot::Active {
                run_id: run_id.to_string(),
            };
            state.deferred = false;
            true
        } else {
            state.deferred = true;
            false
        }
    }

    /// 丢弃延迟启动后清除待重试标记。
    ///
    /// `deferred` 与运行循环持有的 `deferred_start` 是同一事实的两面；
    /// 停止边界丢弃 `deferred_start` 时必须同步清除此标记，否则快照
    /// 永远报告待处理工作、空闲实例无法被回收。
    pub(crate) fn clear_deferred(&self) {
        self.state.lock().expect("run gate lock poisoned").deferred = false;
    }

    /// 是否仍有待重试的延迟内部启动；供回收判定与测试观察共用。
    #[cfg(test)]
    pub(crate) fn has_deferred(&self) -> bool {
        self.state.lock().expect("run gate lock poisoned").deferred
    }

    pub(crate) fn begin_maintenance(&self) -> bool {
        let mut state = self.state.lock().expect("run gate lock poisoned");
        if state.lifecycle == AgentLifecycle::Active && matches!(state.slot, GateSlot::Free) {
            state.slot = GateSlot::Maintenance;
            true
        } else {
            false
        }
    }

    pub(crate) fn is_free(&self) -> bool {
        matches!(
            self.state.lock().expect("run gate lock poisoned").slot,
            GateSlot::Free
        )
    }

    /// 回收也占用同一资格锁，避免空闲检查后另一个请求已经预留。
    fn close_idle(&self) -> bool {
        let mut state = self.state.lock().expect("run gate lock poisoned");
        if state.lifecycle == AgentLifecycle::Active
            && matches!(state.slot, GateSlot::Free)
            && !state.deferred
        {
            state.lifecycle = AgentLifecycle::Closing;
            self.released.notify_one();
            true
        } else {
            false
        }
    }

    pub(crate) fn finish(&self) {
        let mut state = self.state.lock().expect("run gate lock poisoned");
        if matches!(state.slot, GateSlot::Active { .. } | GateSlot::Maintenance) {
            state.slot = GateSlot::Free;
        }
    }

    fn set_current_run(&self, run: Option<CurrentRun>) {
        if let Some(run) = run {
            let mut state = self.state.lock().expect("run gate lock poisoned");
            assert!(matches!(state.slot, GateSlot::Active { .. }));
            state.slot = GateSlot::Active {
                run_id: run.run_id.to_string(),
            };
        } else {
            self.finish();
        }
    }

    fn snapshot(&self) -> (AgentLifecycle, Option<CurrentRun>, bool, bool) {
        let state = self.state.lock().expect("run gate lock poisoned");
        let run = match &state.slot {
            GateSlot::Active { run_id } => Some(CurrentRun {
                run_id: run_id.clone().into(),
            }),
            _ => None,
        };
        (
            state.lifecycle,
            run,
            matches!(state.slot, GateSlot::Reserved { .. }),
            state.deferred || matches!(state.slot, GateSlot::Maintenance),
        )
    }

    fn set_lifecycle(&self, lifecycle: AgentLifecycle) {
        {
            let mut state = self.state.lock().expect("run gate lock poisoned");
            // 并发关闭只能推进生命周期，较晚的请求不得把 Closed 改回 Closing。
            if state.lifecycle == AgentLifecycle::Closed
                || (state.lifecycle == AgentLifecycle::Closing
                    && lifecycle == AgentLifecycle::Active)
            {
                return;
            }
            state.lifecycle = lifecycle;
        }
        if lifecycle != AgentLifecycle::Active {
            self.closing.notify_waiters();
            self.released.notify_one();
        }
        if lifecycle == AgentLifecycle::Closed {
            self.finished.send_replace(true);
        }
    }

    pub(crate) async fn wait_close_requested(&self) {
        let notified = self.closing.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.snapshot().0 == AgentLifecycle::Active {
            notified.await;
        }
    }

    /// 关闭命令通道后，等待宿主完成已开始的受理事务并释放未启动资格。
    pub(crate) async fn wait_unreserved(&self) {
        loop {
            let notified = self.released.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.snapshot().2 {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn wait_closed(&self) -> Result<(), CoreError> {
        self.finished
            .subscribe()
            .wait_for(|closed| *closed)
            .await
            .map(|_| ())
            .map_err(|_| CoreError::RuntimeClosed)
    }
}

/// 已预留的运行资格。
///
/// `commit` 确认启动后失效；未提交即被丢弃（持久化失败或请求中断）时，
/// `Drop` 自动释放预留，保证不泄漏。
pub struct RunReservation {
    run_id: crate::execution::RunId,
    handle: AgentHandle,
    armed: bool,
}

impl Drop for RunReservation {
    fn drop(&mut self) {
        if self.armed {
            self.handle.gate.release(self.run_id.as_str());
        }
    }
}

impl RunReservation {
    /// 本次预留对应的真实 RunId；宿主在预留窗口内持久化初始 Run 记录时使用。
    pub fn run_id(&self) -> &crate::execution::RunId {
        &self.run_id
    }

    /// 确认启动：实例受理（并激活闸门）后运行以该 RunId 执行。
    ///
    /// 展示输入与初始 Run 记录必须已持久化成功；失败路径直接丢弃本预留即可。
    pub async fn commit(
        mut self,
        message: omini_model::message::Message,
    ) -> Result<crate::execution::RunId, CoreError> {
        let run_id = self.run_id.clone();
        let result = self
            .handle
            .send_command(|ack| AgentCommand::CommitRun {
                run_id: run_id.to_string(),
                message,
                ack,
            })
            .await;
        match result {
            // 实例受理即已激活闸门，预留到此消费完毕。
            Ok(()) => {
                self.armed = false;
                Ok(run_id)
            }
            Err(error) => Err(error),
        }
    }
}

/// 实例任务与句柄共享的快照状态。
///
/// 实例任务是唯一写者（配置、运行、生命周期），句柄只读；配置在实例内
/// 修改后立即发布到这里的镜像，保证查询读到的是执行实例实际采用的状态。
pub(crate) struct InstanceState {
    gate: Arc<RunGate>,
    settings: RwLock<Settings>,
    active_profile: Arc<RwLock<ActiveProfile>>,
    pending_tool_pauses: PendingToolPauses,
    supervisor: RwLock<Option<Arc<AgentTaskSupervisor>>>,
}

impl InstanceState {
    pub(crate) fn new(
        settings: Settings,
        active_profile: ActiveProfile,
        gate: Arc<RunGate>,
    ) -> Self {
        Self {
            gate,
            settings: RwLock::new(settings),
            active_profile: Arc::new(RwLock::new(active_profile)),
            pending_tool_pauses: Arc::new(Mutex::new(std::collections::HashMap::new())),
            supervisor: RwLock::new(None),
        }
    }

    /// 与执行实例共享的活跃配置句柄；快照读取实时值。
    pub(crate) fn active_profile_handle(&self) -> Arc<RwLock<ActiveProfile>> {
        Arc::clone(&self.active_profile)
    }

    pub(crate) fn active_profile(&self) -> ActiveProfile {
        *self
            .active_profile
            .read()
            .expect("active profile lock poisoned")
    }

    pub(crate) fn publish_settings(&self, settings: &Settings) {
        *self
            .settings
            .write()
            .expect("instance settings lock poisoned") = settings.clone();
    }

    pub(crate) fn read_settings(&self) -> Settings {
        self.settings
            .read()
            .expect("instance settings lock poisoned")
            .clone()
    }

    pub(crate) fn set_lifecycle(&self, lifecycle: AgentLifecycle) {
        self.gate.set_lifecycle(lifecycle);
    }

    pub(crate) fn lifecycle(&self) -> AgentLifecycle {
        self.gate.snapshot().0
    }

    pub(crate) fn set_current_run(&self, run: Option<CurrentRun>) {
        self.gate.set_current_run(run);
    }

    pub(crate) fn current_run(&self) -> Option<CurrentRun> {
        self.gate.snapshot().1
    }

    pub(crate) fn pending_tool_pauses(&self) -> &PendingToolPauses {
        &self.pending_tool_pauses
    }

    pub(crate) fn attach_supervisor(&self, supervisor: Arc<AgentTaskSupervisor>) {
        *self.supervisor.write().expect("supervisor lock poisoned") = Some(supervisor);
    }

    /// 收尾完成后解除对任务监督器的引用。
    ///
    /// supervisor 持有输出通道的发送端克隆，而 state 可经句柄长期存活；
    /// 不拆除会让输出通道在实例任务结束后保持打开，消费者收不到通道
    /// 关闭（`recv` 不返回 `None`），违反「Closed 之后输出随即关闭」契约。
    pub(crate) fn detach_supervisor(&self) {
        *self.supervisor.write().expect("supervisor lock poisoned") = None;
    }

    pub(crate) fn supervisor(&self) -> Option<Arc<AgentTaskSupervisor>> {
        self.supervisor
            .read()
            .expect("supervisor lock poisoned")
            .clone()
    }
}

/// Agent 实例的命令句柄：运行提交、干预、取消、配置修改与查询。
///
/// 可克隆；命令经有界通道进入实例任务并等待确认，句柄不持有实例任务所有权。
#[derive(Clone)]
pub struct AgentHandle {
    thread_id: String,
    cmd_tx: mpsc::Sender<AgentCommand>,
    gate: Arc<RunGate>,
    state: Arc<InstanceState>,
    capabilities: Arc<CapabilityStore>,
    mcp_manager: Arc<McpManager>,
}

impl AgentHandle {
    pub(crate) fn new(
        thread_id: String,
        cmd_tx: mpsc::Sender<AgentCommand>,
        gate: Arc<RunGate>,
        state: Arc<InstanceState>,
        capabilities: Arc<CapabilityStore>,
        mcp_manager: Arc<McpManager>,
    ) -> Self {
        Self {
            thread_id,
            cmd_tx,
            gate,
            state,
            capabilities,
            mcp_manager,
        }
    }

    /// 判断两个句柄是否属于同一次装配；同一宿主会话重新加载后是不同实例。
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.gate, &other.gate)
    }

    /// 仅在没有运行资格、任务或待提交通知时开始回收；与新预留原子互斥。
    ///
    /// 判定条件与 [`AgentSnapshot::is_reclaimable`] 相同，但判定与转换在
    /// 资格锁内一次完成——先查快照再回收会在两步之间漏入新预留；
    /// 两处条件需同步演进。
    pub fn begin_idle_close(&self) -> bool {
        if self.state.supervisor().is_some_and(|supervisor| {
            supervisor.has_active_tasks() || supervisor.task_manager().has_pending_notifications()
        }) {
            return false;
        }
        self.gate.close_idle()
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// 权威实例快照：生命周期、运行、有效配置、待交互与可回收性。
    pub fn snapshot(&self) -> AgentSnapshot {
        let settings = self.state.read_settings();
        let model = settings.active_model();
        let (lifecycle, current_run, run_reserved, has_pending_work) = self.gate.snapshot();
        AgentSnapshot {
            lifecycle,
            current_run,
            run_reserved,
            has_pending_work: has_pending_work
                || self.state.supervisor().is_some_and(|supervisor| {
                    supervisor.task_manager().has_pending_notifications()
                }),
            has_active_tasks: self
                .state
                .supervisor()
                .is_some_and(|supervisor| supervisor.has_active_tasks()),
            pending_tool_pauses: self
                .state
                .pending_tool_pauses()
                .lock()
                .expect("pending tool pause mutex poisoned")
                .keys()
                .cloned()
                .collect(),
            active_profile: self.state.active_profile(),
            provider: model.provider_id.clone(),
            model: model.model_id.clone(),
            thinking_effort: model.thinking_effort,
        }
    }

    /// 校验并构建用户输入的 LLM 上下文消息（skill 展开、init 注入、附件编码）。
    ///
    /// 读取实例当前有效配置，不经独立副本。用户时间线记录由宿主在接收路径构建。
    pub fn prepare_run(
        &self,
        command: thread_types::SubmitRunCommand,
    ) -> Result<thread_types::PreparedRunCommand, CoreError> {
        let thread_types::SubmitRunCommand {
            input,
            client_echo_id,
            intent,
        } = command;
        let execute_command = match intent {
            thread_types::RunIntent::ExecuteCommand(command) => Some(command),
            thread_types::RunIntent::SubmitMessage | thread_types::RunIntent::InterveneMessage => {
                None
            }
        };
        let settings = self.state.read_settings();
        let message = user_input::prepare_submission(
            input.clone(),
            execute_command,
            &settings,
            &self.capabilities,
        )?;
        Ok(thread_types::PreparedRunCommand {
            message,
            input,
            client_echo_id,
            intent,
        })
    }

    /// 原子预留运行资格并分配真实 RunId。
    ///
    /// 存在运行预留、活跃运行或实例收尾时返回 `run_busy`，宿主据此拒绝
    /// 新提交且不保存被拒绝输入。
    pub fn reserve_run(&self) -> Result<RunReservation, CoreError> {
        let run_id = self.gate.reserve().ok_or_else(CoreError::run_busy)?;
        Ok(RunReservation {
            run_id: run_id.into(),
            handle: self.clone(),
            armed: true,
        })
    }

    /// 干预当前主 Run：消息在安全输入边界注入。
    pub async fn intervene(&self, message: omini_model::message::Message) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::Intervene {
            run_id: None,
            message,
            client_source: None,
            ack,
        })
        .await
    }

    /// 向子 Run 投递已登记来源键的输入；实际注入由安全边界确认。
    pub async fn intervene_agent_run(
        &self,
        run_id: String,
        message: omini_model::message::Message,
        client_source: Option<omini_runtime_contract::thread_domain::ClientMessage>,
    ) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::Intervene {
            run_id: Some(run_id),
            message,
            client_source,
            ack,
        })
        .await
    }

    /// 取消当前主 Run 及其任务树。
    pub async fn cancel_current(&self) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::Cancel { run_id: None, ack })
            .await
    }

    /// 取消指定子 Run 及其后代。
    pub async fn cancel_agent_run(&self, run_id: String) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::Cancel {
            run_id: Some(run_id),
            ack,
        })
        .await
    }

    pub async fn compact_context(&self, instructions: Option<String>) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::CompactContext { instructions, ack })
            .await
    }

    pub async fn set_model(&self, command: thread_types::SetModelCommand) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::SetModel {
            provider: command.provider,
            model: command.model,
            thinking_effort: command.thinking_effort,
            ack,
        })
        .await
    }

    pub async fn set_thinking_effort(
        &self,
        command: thread_types::SetThinkingEffortCommand,
    ) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::SetThinkingEffort {
            effort: command.effort,
            ack,
        })
        .await
    }

    pub async fn set_active_profile(&self, profile: ActiveProfile) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::SetActiveProfile { profile, ack })
            .await
    }

    pub async fn toggle_active_profile(&self) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::ToggleActiveProfile { ack })
            .await
    }

    pub async fn resolve_tool_pause(
        &self,
        command: thread_types::ResolveToolPauseCommand,
    ) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::ResolveToolPause {
            tool_use_id: command.tool_use_id,
            response: command.response,
            ack,
        })
        .await
    }

    pub async fn resolve_plan(
        &self,
        command: thread_types::ResolvePlanCommand,
    ) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::ResolvePlanApproval {
            plan_id: command.plan_id,
            action: command.action,
            ack,
        })
        .await
    }

    pub async fn reload_subagent_registry(&self) -> Result<(), CoreError> {
        self.send_command(|ack| AgentCommand::ReloadSubagentRegistry { ack })
            .await
    }

    /// 发起关闭：停止接收新工作，取消前台运行与任务树并收尾。幂等。
    ///
    /// 返回前等待实例执行、任务与必要持久化收尾，终止公告已进入输出。
    /// 宿主必须提交或丢弃仍持有的预留；关闭会等待该受理窗口结算。
    /// 已进入收尾的实例直接确认成功；实例在命令投递与确认之间退出时，
    /// 只要收尾已启动也按幂等成功处理。
    pub async fn close(&self) -> Result<(), CoreError> {
        self.request_close().await?;
        self.gate.wait_closed().await
    }

    /// 仅请求开始关闭；输出消费者自身使用此入口，以便继续排空尾部事件。
    pub async fn request_close(&self) -> Result<(), CoreError> {
        if self.state.lifecycle() != AgentLifecycle::Active {
            return Ok(());
        }
        // 在命令处理前封住新预留，关闭可中断 MCP 等尚未进入 query 的等待。
        self.state.set_lifecycle(AgentLifecycle::Closing);
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(AgentCommand::Close { ack: ack_tx })
            .await
            .is_err()
        {
            return if self.state.lifecycle() != AgentLifecycle::Active {
                Ok(())
            } else {
                Err(CoreError::RuntimeClosed)
            };
        }
        match ack_rx.await {
            Ok(result) => result,
            Err(_) if self.state.lifecycle() != AgentLifecycle::Active => Ok(()),
            Err(_) => Err(CoreError::RuntimeClosed),
        }
    }

    // ===== 只读能力查询（读取实例共享的真实状态） =====

    pub fn list_models(&self) -> thread_types::ModelsSnapshot {
        let settings = self.state.read_settings();
        let mut providers = settings.resolved_config().catalog();
        providers.sort_by(|a, b| a.id.cmp(&b.id));
        let model = settings.active_model();
        thread_types::ModelsSnapshot {
            providers,
            current_provider: model.provider_id.clone(),
            current_model: model.model_id.clone(),
        }
    }

    pub fn list_skills(&self) -> Vec<thread_types::SkillSummarySnapshot> {
        crate::user_invocable_skill_summaries(&self.capabilities.skill_registry())
    }

    pub fn runtime_skills(&self) -> Vec<thread_types::RuntimeSkillSnapshot> {
        crate::runtime_skill_snapshots(&self.capabilities)
    }

    pub fn runtime_mcp_servers(
        &self,
    ) -> Vec<omini_runtime_contract::mcp::RuntimeMcpServerSnapshot> {
        self.mcp_manager.runtime_snapshots()
    }

    pub fn runtime_subagents(&self) -> Vec<omini_domain::subagents::AgentSummary> {
        self.capabilities.subagent_registry().summaries()
    }

    /// 发送命令并等待确认；通道关闭视为实例不可用。
    async fn send_command<F>(&self, build: F) -> Result<(), CoreError>
    where
        F: FnOnce(oneshot::Sender<Result<(), CoreError>>) -> AgentCommand,
    {
        let (ack_tx, ack_rx) = oneshot::channel();
        let command = build(ack_tx);
        self.cmd_tx
            .send(command)
            .await
            .map_err(|_| CoreError::RuntimeClosed)?;
        match ack_rx.await {
            Ok(result) => result,
            // 实例任务在确认前退出：命令未完成，按实例不可用处理。
            Err(_) => Err(CoreError::RuntimeClosed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_concurrently() {
        // 给定两个同时到达的提交，当竞争同一闸门，则仅一个取得运行资格。
        let gate = Arc::new(RunGate::default());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let admitted = std::thread::scope(|scope| {
            let attempts = (0..2)
                .map(|_| {
                    let gate = Arc::clone(&gate);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        gate.reserve().is_some()
                    })
                })
                .collect::<Vec<_>>();
            attempts
                .into_iter()
                .map(|attempt| usize::from(attempt.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(admitted, 1);
    }

    #[test]
    fn defer_internal_run() {
        // 给定宿主已预留，当完成通知尝试续跑，则不能抢占；提交后也不能重复受理。
        let gate = RunGate::default();
        let reserved = gate.reserve().unwrap();
        assert!(!gate.start_internal("notification"));
        assert!(gate.snapshot().2);
        assert!(gate.activate(&reserved));
        assert_eq!(gate.snapshot().1.unwrap().run_id.as_str(), reserved);
        assert!(gate.reserve().is_none());
        gate.finish();
        assert!(gate.start_internal("continuation"));
        assert!(gate.reserve().is_none());
    }

    #[tokio::test]
    async fn await_reserved_acceptance() {
        // 给定宿主尚在受理窗口，当开始关闭，则等待预留释放且不再允许激活。
        use std::future::Future;
        let gate = RunGate::default();
        let reserved = gate.reserve().unwrap();
        gate.set_lifecycle(AgentLifecycle::Closing);
        assert!(!gate.activate(&reserved));
        let waiting = gate.wait_unreserved();
        tokio::pin!(waiting);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(waiting.as_mut().poll(&mut context).is_pending());
        gate.release(&reserved);
        assert!(waiting.as_mut().poll(&mut context).is_ready());
        gate.set_lifecycle(AgentLifecycle::Closed);
        gate.set_lifecycle(AgentLifecycle::Closing);
        assert_eq!(gate.snapshot().0, AgentLifecycle::Closed);
    }

    #[tokio::test]
    async fn await_close_completion() {
        // 给定正在收尾，当等待关闭，则必须保持 Pending，只有完成公告才唤醒。
        use std::future::Future;
        let gate = RunGate::default();
        gate.set_lifecycle(AgentLifecycle::Closing);
        assert!(gate.reserve().is_none());
        let waiting = gate.wait_closed();
        tokio::pin!(waiting);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(waiting.as_mut().poll(&mut context).is_pending());
        gate.set_lifecycle(AgentLifecycle::Closed);
        assert!(waiting.as_mut().poll(&mut context).is_ready());
    }
}
