use super::service::{AgentRuntime, RunStart};
use super::*;
use crate::execution::handle::AgentOutput;
use crate::execution::snapshot::{AgentLifecycle, CurrentRun};
use crate::runtime::command::AgentCommand;
use omini_domain::conversation::TaskNotification;
use omini_domain::task::TaskCompletion;
use tracing::Instrument;

impl AgentRuntime {
    /// 启动实例任务；实例生命周期由 `AgentInstance` 持有本句柄。
    pub(crate) fn spawn_task(mut self) -> tokio::task::JoinHandle<()> {
        let thread_id = self.thread_id.clone();
        tokio::spawn(
            async move {
                tracing::debug!("agent instance task started");
                self.run_loop().await;
                tracing::debug!("agent instance task stopped");
            }
            .instrument(tracing::debug_span!(
                "agent_instance",
                thread_id = %thread_id,
                task_kind = "agent_instance"
            )),
        )
    }

    /// 实例主循环：空闲公告、命令分发、任务完成续跑与输出断开收尾。
    async fn run_loop(&mut self) {
        for diagnostic in std::mem::take(&mut self.initial_diagnostics) {
            if !self.output.send_event(diagnostic).await {
                break;
            }
        }
        self.start_mcp_initialization();
        loop {
            if self.state.lifecycle() != AgentLifecycle::Active {
                break;
            }
            if self.gate.is_free()
                && let Some(start) = self.deferred_start.take()
            {
                // 停止边界内丢弃被推迟的通知续跑：延迟启动只承载自动唤醒，
                // 边界已撤销其资格。必须同步清除闸门的待重试标记，否则
                // 实例快照永远报告待处理工作，无法回到可回收的空闲态。
                if self.task_supervisor.stop_boundary_open() {
                    self.gate.clear_deferred();
                    continue;
                }
                self.process_run(start, None).await;
                continue;
            }
            // 空闲判定来自实例实际状态（无预留、无运行、无未完成任务）；
            // select 每次返回都意味着新活动，因此每个空闲期至多公告一次。
            // 宿主据此回收，不再依赖固定延时。
            if self.is_idle() {
                self.output.send(AgentOutput::Idle).await;
            }
            tokio::select! {
                command = self.cmd_rx.recv() => {
                    let Some(command) = command else { break };
                    if !self.handle_idle_command(command).await {
                        break;
                    }
                }
                Some(completion) = self.task_completion_rx.recv() => {
                    // 停止边界内的完成：立即结算持久化并入收件箱，既不进入
                    // 引擎队列也不唤醒主运行；边界外的完成保持空闲自动续跑。
                    if self
                        .task_supervisor
                        .completion_is_stopped(&completion.task_id)
                    {
                        self.persist_stopped_completions(vec![completion]).await;
                        continue;
                    }
                    self.query_engine.enqueue_task_completion(completion);
                    self.collect_task_completions().await;
                    if self.state.lifecycle() == AgentLifecycle::Active
                        && !self.output.is_closed()
                    {
                        self.process_run(RunStart::PendingTaskNotification, None).await;
                    }
                }
                // 输出消费者断开：实例失去宿主，立即收尾。
                // closed() 本身只会完成一次，无需再以标志守卫（send 失败
                // 置位的 closed 标志反而会错误地禁用本分支）。
                () = self.output.closed() => {
                    tracing::debug!("agent output consumer disconnected");
                    break;
                }
                // 预留被丢弃（持久化失败路径）：回到循环顶部重新公告空闲，
                // 无客户端的会话才能被消费者回收。
                () = self.gate.wait_reservation_released() => {}
            }
        }
        self.shutdown().await;
        let _ = self.output.send(AgentOutput::Closed).await;
        self.state.set_lifecycle(AgentLifecycle::Closed);
    }

    /// 确认后的主停止：先建立停止边界撤销全部自动唤醒资格，再发出取消
    /// 信号并取消当前运行与整棵后台任务树。
    ///
    /// 边界必须先于取消信号建立——取消后陆续完成或仍在注册的后代都属于
    /// 被停止的工作，其通知只结算持久化，不再驱动任何主运行。
    pub(crate) async fn stop_main_work(&self) {
        self.task_supervisor.open_stop_boundary();
        self.cancelled.store(true, Ordering::Relaxed);
        self.query_engine.notify_cancel_waiters();
        self.task_supervisor.cancel_all().await;
    }

    /// 运行中的完成接收：停止完成立即结算入箱（不进入引擎队列，也就
    /// 不会以 had_task_notification 触发本次运行的额外续转），边界外
    /// 完成照常排队等待 Turn 边界的引擎排空。
    async fn handle_run_completion(&self, completion: TaskCompletion) {
        if self
            .task_supervisor
            .completion_is_stopped(&completion.task_id)
        {
            self.persist_stopped_completions(vec![completion]).await;
        } else {
            self.query_engine.enqueue_task_completion(completion);
        }
    }

    /// 把停止边界内的完成通知立即结算落库并送入收件箱。
    ///
    /// 通知在完成时刻持久化并广播（UI 及时可见），但不创建主运行、不发起
    /// Provider 请求。完成进入独立收件箱而非引擎队列：与引擎排空隔离，
    /// 旧完成既不会在运行边界触发额外续跑（follow_up），也不会被重复注入。
    /// 落库失败时同样入箱，由 [`Self::flush_stopped_inbox`] 在下一次显式
    /// 输入前重试，通知不丢失。
    async fn persist_stopped_completions(&self, completions: Vec<TaskCompletion>) {
        if completions.is_empty() {
            return;
        }
        let task_ids = completions
            .iter()
            .map(|completion| completion.task_id.clone())
            .collect::<Vec<_>>();
        let notification = TaskNotification {
            tasks: completions.clone(),
            created_at: Timestamp::now(),
        };
        match self
            .host
            .insert_task_notification(&self.thread_id, &notification)
            .await
        {
            // 成功确认即提交的全部任务已交付（新内容本次写入、其余此前
            // 已写入），全部提交身份从在途集合退役；重复/部分重叠结算
            // 不会在待交付集合中留下永不清理的身份。注入收件箱仍按
            // completions 全量去重进行。
            Ok(_) => {
                self.task_supervisor.mark_notifications_delivered(&task_ids);
            }
            Err(error) => {
                // 失败保留在途身份，由下一次显式输入前的冲刷重试。
                tracing::warn!(thread_id = %self.thread_id, %error, "failed to settle stopped task notification");
                let _ = self
                    .output
                    .send_event(RuntimeToServerEvent::warning(format!(
                        "后台任务通知暂时未能落库：{error}"
                    )))
                    .await;
            }
        }
        for completion in completions {
            self.task_supervisor.enqueue_stopped_completion(completion);
        }
    }

    /// 结算引擎队列中残留的完成（被取消运行尚未消费的入队完成）。
    ///
    /// 它们完成于取消前后，同属被停止的当前工作：只结算持久化入箱，
    /// 不再触发任何续跑；内存补齐由下一次显式输入前的收件箱冲刷完成。
    async fn settle_stopped_completions(&mut self) {
        let pending = self.query_engine.take_pending_completions();
        self.persist_stopped_completions(pending).await;
    }

    /// 冲刷停止收件箱：已持久化的通知补进内存模型上下文，未持久化的
    /// 就地重试一次，仍失败才交还引擎队列。
    ///
    /// 调用点都在“停止边界已关闭、新上下文即将建立”之前：
    /// - 新的显式用户输入——通知按到达顺序排在新用户消息之前；
    /// - 手动压缩——压缩以新 context_version 整体重写上下文，不冲刷的话
    ///   已落库未补内存的通知将在重写后从上下文中永久丢失。
    pub(crate) async fn flush_stopped_inbox(&mut self) {
        let inbox = self.task_supervisor.take_stopped_inbox();
        if inbox.is_empty() {
            return;
        }
        let mut persisted = Vec::new();
        let mut retry = Vec::new();
        for completion in inbox {
            if self
                .task_supervisor
                .notification_is_delivered(&completion.task_id)
            {
                persisted.push(completion);
            } else {
                retry.push(completion);
            }
        }
        if !persisted.is_empty() {
            self.push_notification_message(&persisted);
        }
        if !retry.is_empty() {
            let notification = TaskNotification {
                tasks: retry.clone(),
                created_at: Timestamp::now(),
            };
            match self
                .host
                .insert_task_notification(&self.thread_id, &notification)
                .await
            {
                Ok(Some(fresh)) => {
                    let fresh_ids = fresh
                        .tasks
                        .iter()
                        .map(|completion| completion.task_id.clone())
                        .collect::<Vec<_>>();
                    self.task_supervisor
                        .mark_notifications_delivered(&fresh_ids);
                    self.push_notification_message(&retry);
                }
                Ok(None) => {
                    // 重试前被其它路径交付：直接补内存。
                    self.push_notification_message(&retry);
                }
                Err(error) => {
                    tracing::warn!(thread_id = %self.thread_id, %error, "deferred retry of stopped task notification failed");
                    // 仍未落库：交还引擎队列，由本次运行的引擎排空继续重试。
                    // 注入位置退化为用户消息之后，属罕见失败路径的取舍。
                    self.query_engine.restore_pending_completions(retry);
                }
            }
        }
    }

    /// 把（已确认落库的）通知消息按序追加进内存模型上下文。
    fn push_notification_message(&mut self, completions: &[TaskCompletion]) {
        let task_ids = completions
            .iter()
            .map(|completion| completion.task_id.clone())
            .collect::<Vec<_>>();
        self.messages.push(Message::from_user_text(
            omini_domain::conversation::task_notification_text(completions),
        ));
        // 幂等清理未交付标记；正常路径在结算时已移除，这里通常无操作。
        self.task_supervisor.mark_notifications_delivered(&task_ids);
    }

    /// 实例是否处于可回收空闲：无运行、无预留、无未完成任务且输出仍连接。
    fn is_idle(&self) -> bool {
        self.gate.is_free()
            && self.deferred_start.is_none()
            && self.task_completion_rx.is_empty()
            && !self.task_supervisor.has_active_tasks()
            && !self.output.is_closed()
    }

    /// 关闭收尾：取消前台运行与任务树、等待任务退出与尾部输出入队。
    async fn shutdown(&mut self) {
        self.state.set_lifecycle(AgentLifecycle::Closing);
        // 拒绝并丢弃未处理的确认端，使宿主受理任务能够失败结算并释放预留。
        self.cmd_rx.close();
        while let Ok(command) = self.cmd_rx.try_recv() {
            drop(command);
        }
        self.gate.wait_unreserved().await;
        self.cancelled.store(true, Ordering::Relaxed);
        self.query_engine.notify_cancel_waiters();
        self.task_supervisor.cancel_all().await;
        self.task_supervisor.wait_until_idle().await;
        // 任务树全部结束后还有最后一批尾部完成：它们的通知此刻尚未结算，
        // 不落库的话重启恢复会把未投递通知重新当作自动唤醒源。
        let mut tail = Vec::new();
        while let Ok(completion) = self.task_completion_rx.try_recv() {
            tail.push(completion);
        }
        tail.extend(self.query_engine.take_pending_completions());
        self.persist_stopped_completions(tail).await;
        // 任务全部结束后解除 state 对 supervisor 的引用，释放其持有的
        // 输出通道克隆；实例任务退出后通道才能随之关闭。
        if let Some(initialization) = self.mcp_initialization.take() {
            initialization.abort();
            let _ = initialization.await;
        }
        self.state.detach_supervisor();
    }

    /// 处理空闲态命令；返回 `false` 表示实例应当退出（关闭）。
    async fn handle_idle_command(&mut self, command: AgentCommand) -> bool {
        match command {
            AgentCommand::CommitRun {
                run_id,
                message,
                ack,
            } => {
                // 展示输入与初始 Run 记录已由宿主在预留窗口持久化；
                // 激活闸门后以真实 RunId 执行。
                if self.gate.activate(&run_id) {
                    self.deferred_start = None;
                    let _ = ack.send(Ok(()));
                    // 新的显式用户输入开启新代：关闭停止边界，此后注册与
                    // 完成的任务恢复自动唤醒资格（旧停止集合不受影响）。
                    self.task_supervisor.close_stop_boundary();
                    // 同步复位主取消标志：此前的主停止（含压缩期间取消）
                    // 不得把取消状态带进新的显式工作。旧任务自身的取消
                    // 令牌独立持有，不受影响。
                    self.cancelled.store(false, Ordering::Relaxed);
                    // 收件箱通知在用户消息进入上下文之前冲刷,保持内存
                    // 顺序与落库历史一致。
                    self.flush_stopped_inbox().await;
                    self.submit_user_message(message, run_id).await;
                } else {
                    let _ = ack.send(Err(CoreError::RuntimeClosed));
                }
                true
            }
            AgentCommand::Intervene {
                run_id: None, ack, ..
            } => {
                let _ = ack.send(Err(CoreError::new(
                    "Cannot intervene because no run is active",
                )));
                true
            }
            AgentCommand::Intervene {
                run_id: Some(run_id),
                message,
                client_source,
                ack,
            } => {
                let _ = ack.send(
                    self.task_supervisor
                        .intervene_agent_run(&run_id, message, client_source)
                        .await
                        .map_err(CoreError::new),
                );
                true
            }
            AgentCommand::Cancel { run_id: None, ack } => {
                // 空闲态主停止：当前无前台运行，只撤销后台任务的唤醒资格
                // 并取消任务树；不置 cancelled 标志，避免影响下一次运行
                // 的取消判定。
                self.task_supervisor.open_stop_boundary();
                self.task_supervisor.cancel_all().await;
                // 延迟启动可能已将完成移入引擎队列；停止后立即结算，
                // 避免丢弃启动时把通知留到下一次输入才落库。
                self.settle_stopped_completions().await;
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::Cancel {
                run_id: Some(run_id),
                ack,
            } => {
                // cancel_task 以 ToolResult 表达成败：错误时把输出作为原因返回。
                let result = self.task_supervisor.cancel_task(&run_id).await;
                let _ = ack.send(if result.is_error {
                    Err(CoreError::new(result.output))
                } else {
                    Ok(())
                });
                true
            }
            AgentCommand::CompactContext { instructions, ack } => {
                if self.gate.begin_maintenance() {
                    self.handle_compact_context(instructions).await;
                    self.gate.finish();
                    let _ = ack.send(Ok(()));
                } else {
                    let _ = ack.send(Err(CoreError::run_busy()));
                }
                true
            }
            AgentCommand::SetModel {
                provider,
                model,
                thinking_effort,
                ack,
            } => {
                let result = self.switch_model(&provider, &model, thinking_effort).await;
                let _ = ack.send(result);
                true
            }
            AgentCommand::SetThinkingEffort { effort, ack } => {
                let _ = ack.send(self.apply_thinking_effort(effort).await);
                true
            }
            AgentCommand::ToggleActiveProfile { ack } => {
                self.toggle_active_profile().await;
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::SetActiveProfile { profile, ack } => {
                self.set_active_profile(profile);
                self.send_event(RuntimeToServerEvent::ActiveProfileChanged(
                    self.active_profile(),
                ))
                .await;
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::ResolveToolPause {
                tool_use_id,
                response,
                ack,
            } => {
                if let Err(error) = self.query_engine.resolve_tool_pause(&tool_use_id, response) {
                    tracing::debug!(tool_use_id, %error, "stale tool pause resolution ignored");
                }
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::ResolvePlanApproval {
                plan_id,
                action,
                ack,
            } => {
                self.resolve_plan_approval(&plan_id, action).await;
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::ReloadSubagentRegistry { ack } => {
                self.reload_subagent_registry();
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::Close { ack } => {
                // 关闭幂等且运行中不拒绝：确认语义为收尾已启动。
                self.state.set_lifecycle(AgentLifecycle::Closing);
                let _ = ack.send(Ok(()));
                false
            }
        }
    }

    /// 处理运行中命令；返回 `false` 表示运行应当终止并收尾（关闭/输出断开）。
    ///
    /// 只访问共享句柄（引擎、监督器、输出、宿主），与查询 future 持有的
    /// `messages`/`query_engine` 借用不相交；配置命令在查询循环内按字段另行处理。
    async fn handle_active_command(&self, command: AgentCommand) -> bool {
        match command {
            AgentCommand::CommitRun { ack, .. } => {
                // 闸门保证运行中不会出现合法的预留提交；按实例不可用拒绝，
                // 预留方收到错误后自动释放。
                let _ = ack.send(Err(CoreError::RuntimeClosed));
                true
            }
            AgentCommand::Intervene {
                run_id: None,
                message,
                ack,
                ..
            } => {
                tracing::debug!(
                    request_kind = "intervene_message",
                    "active run intervention received"
                );
                self.query_engine.enqueue_user_message(message);
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::Intervene {
                run_id: Some(run_id),
                message,
                client_source,
                ack,
            } => {
                let result = self
                    .task_supervisor
                    .intervene_agent_run(&run_id, message, client_source)
                    .await
                    .map_err(CoreError::new);
                let _ = ack.send(result);
                true
            }
            AgentCommand::Cancel { run_id: None, ack } => {
                tracing::debug!("active run cancellation requested");
                self.stop_main_work().await;
                let _ = ack.send(Ok(()));
                true
            }
            AgentCommand::Cancel {
                run_id: Some(run_id),
                ack,
            } => {
                // cancel_task 以 ToolResult 表达成败：错误时把输出作为原因返回。
                let result = self.task_supervisor.cancel_task(&run_id).await;
                let _ = ack.send(if result.is_error {
                    Err(CoreError::new(result.output))
                } else {
                    Ok(())
                });
                true
            }
            AgentCommand::ResolveToolPause {
                tool_use_id,
                response,
                ack,
            } => {
                tracing::debug!(tool_use_id = %tool_use_id, response = ?response, "resolving tool pause");
                let permission_response = matches!(
                    response,
                    omini_runtime_contract::thread_domain::ToolPauseResponse::Permission { .. }
                );
                let result = match self.query_engine.resolve_tool_pause(&tool_use_id, response) {
                    Ok(()) => {
                        if permission_response && let Some(run_id) = self.state.current_run() {
                            let _ = self
                                .host
                                .update_agent_run(
                                    run_id.run_id.as_str(),
                                    omini_domain::agent_run::AgentRunStatus::Running,
                                    None,
                                    None,
                                    0,
                                )
                                .await
                                .map_err(|error| {
                                    tracing::warn!(%error, "failed to record approval in run");
                                });
                        }
                        Ok(())
                    }
                    Err(error) => Err(CoreError::new(error.to_string())),
                };
                let _ = ack.send(result);
                true
            }
            AgentCommand::ReloadSubagentRegistry { ack } => {
                let _ = ack.send(Err(CoreError::new(
                    "Cannot reload the subagent registry while a run is active",
                )));
                true
            }
            // 配置命令由查询循环按字段借用另行处理，正常不会进入这里；
            // 防御式拒绝保证完备。
            AgentCommand::SetModel { ack, .. }
            | AgentCommand::SetThinkingEffort { ack, .. }
            | AgentCommand::SetActiveProfile { ack, .. }
            | AgentCommand::ToggleActiveProfile { ack } => {
                let _ = ack.send(Err(CoreError::new(
                    "Cannot change configuration while a run is active",
                )));
                true
            }
            AgentCommand::CompactContext { ack, .. } => {
                let _ = ack.send(Err(CoreError::new(
                    "Cannot compact the context while a run is active",
                )));
                true
            }
            AgentCommand::ResolvePlanApproval { ack, .. } => {
                let _ = ack.send(Err(CoreError::new(
                    "Cannot resolve plan approval while a run is active",
                )));
                true
            }
            AgentCommand::Close { ack } => {
                // 运行中不拒绝关闭：确认收尾启动，取消前台运行与任务树。
                tracing::debug!("close requested during active run");
                self.state.set_lifecycle(AgentLifecycle::Closing);
                let _ = ack.send(Ok(()));
                self.cancelled.store(true, Ordering::Relaxed);
                self.query_engine.notify_cancel_waiters();
                self.task_supervisor.cancel_all().await;
                false
            }
        }
    }

    /// 切换模型 / 提供商；确认语义为已应用且必要持久化完成。
    async fn switch_model(
        &mut self,
        provider: &str,
        model: &str,
        thinking_effort: Option<ThinkingEffort>,
    ) -> Result<(), CoreError> {
        let result = active_run::apply_model_selection(
            &mut self.settings,
            &mut self.llm_client,
            &self.project,
            Some(&self.thread_id),
            active_run::ModelSelection {
                provider,
                model,
                thinking_effort,
            },
            active_run::RuntimeSinks {
                output: &self.output,
                host: self.host.as_ref(),
                usage_state: &self.thread_usage,
            },
        )
        .await;
        if result.is_ok() {
            self.publish_config();
        }
        result
    }

    /// 应用思考强度；持久化成功后才改配置，失败返回错误且快照不变。
    async fn apply_thinking_effort(&mut self, effort: ThinkingEffort) -> Result<(), CoreError> {
        let result = active_run::apply_thinking_effort(
            &mut self.settings,
            &self.project,
            Some(&self.thread_id),
            effort,
            &self.output,
            self.host.as_ref(),
        )
        .await;
        self.publish_config();
        result
    }

    pub async fn toggle_active_profile(&mut self) {
        let next = match self.active_profile() {
            ActiveProfile::Main => ActiveProfile::Auto,
            ActiveProfile::Auto => ActiveProfile::Plan,
            ActiveProfile::Plan => ActiveProfile::Main,
        };
        self.set_active_profile(next);
        self.send_event(RuntimeToServerEvent::ActiveProfileChanged(
            self.active_profile(),
        ))
        .await;
    }

    pub fn rebuild_system_prompt(&mut self) {
        let active_profile = self.active_profile();
        active_run::rebuild_system_prompt(&mut self.settings, &self.capabilities, active_profile);
    }

    pub fn reload_subagent_registry(&mut self) {
        self.capabilities.reload_subagents(&self.settings);
        self.rebuild_system_prompt();
        self.publish_config();
    }

    /// 接收一条用户消息，追加进历史并启动一次已预留的运行。
    /// 展示行入库与 echo 已由宿主在接收时完成，这里只管 LLM 上下文。
    async fn submit_user_message(
        &mut self,
        message: omini_model::message::Message,
        reserved_run_id: String,
    ) {
        self.messages.push(message);
        self.process_run(RunStart::UserInput, Some(reserved_run_id))
            .await;
    }

    /// 处理一次完整的运行序列，可能包含多轮 LLM 调用与内部续跑。
    ///
    /// `reserved_run_id` 对应宿主已持久化初始 Run 记录的外部提交；内部续跑
    /// （任务通知、计划批准）自行分配 RunId 并经宿主创建 Run 记录。
    pub(crate) async fn process_run(
        &mut self,
        mut start: RunStart,
        reserved_run_id: Option<String>,
    ) {
        let mut host_created = reserved_run_id.is_none();
        let mut reserved_pending = reserved_run_id;
        if reserved_pending.is_none() {
            let run_id = Uuid::new_v4().to_string();
            if !self.gate.start_internal(&run_id) {
                self.deferred_start = Some(start);
                return;
            }
            reserved_pending = Some(run_id);
        }
        loop {
            self.collect_task_completions().await;
            let run_id = reserved_pending
                .take()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            self.state.set_current_run(Some(CurrentRun {
                run_id: run_id.clone().into(),
            }));
            let created_at = Timestamp::now();
            let mut run_snapshot = omini_domain::agent_run::AgentRunSnapshot {
                id: run_id.clone(),
                thread_id: self.thread_id.clone(),
                parent_run_id: None,
                status: omini_domain::agent_run::AgentRunStatus::Running,
                created_at,
                started_at: Some(created_at),
                finished_at: None,
                total_tokens: 0,
                archived_at: None,
            };
            if host_created {
                // 关键持久化失败时不执行依赖该记录的运行。
                if let Err(error) = self.host.create_agent_run(&run_snapshot).await {
                    tracing::error!(run_id = %run_id, error = %error, "failed to persist agent run");
                    let _ = self
                        .output
                        .send_event(RuntimeToServerEvent::error(error.to_string()))
                        .await;
                    break;
                }
            }
            self.state.set_current_run(Some(CurrentRun {
                run_id: run_id.clone().into(),
            }));
            let _ = self
                .output
                .send_event(RuntimeToServerEvent::AgentRunChanged(run_snapshot.clone()))
                .await;
            if let Err(error) = self.host.touch_thread(&self.thread_id).await {
                tracing::warn!(error = %error, "failed to touch thread");
            }

            let thread_id = self.thread_id.clone();
            let model = self.settings.active_model();
            let run_span = tracing::info_span!(
                "run",
                thread_id = %thread_id,
                run_id = %run_id,
                start_kind = start.kind(),
                provider = %model.provider_id,
                model = %model.model_id,
                thinking_effort = ?model.thinking_effort,
                max_turns = ?self.settings.max_turns,
            );
            let (follow_up, failed, was_cancelled) = self
                .process_run_inner(start, run_id.clone(), thread_id)
                .instrument(run_span)
                .await;
            let status = if was_cancelled {
                omini_domain::agent_run::AgentRunStatus::Cancelled
            } else if failed {
                omini_domain::agent_run::AgentRunStatus::Failed
            } else {
                omini_domain::agent_run::AgentRunStatus::Completed
            };
            run_snapshot.status = status;
            run_snapshot.finished_at = Some(Timestamp::now());
            if let Err(error) = self
                .host
                .update_agent_run(&run_id, status, None, run_snapshot.finished_at, 0)
                .await
            {
                self.state.set_lifecycle(AgentLifecycle::Closing);
                tracing::error!(run_id = %run_id, error = %error, "failed to settle agent run");
                let _ = self
                    .output
                    .send_event(RuntimeToServerEvent::error(error.to_string()))
                    .await;
                break;
            }
            let _ = self
                .output
                .send_event(RuntimeToServerEvent::AgentRunChanged(run_snapshot))
                .await;
            // 整个续跑序列保持运行资格；不暴露尚有续跑工作时的空闲窗口。
            host_created = true;
            let collected_after_run = self.collect_task_completions().await;
            if self.state.lifecycle() == AgentLifecycle::Closing || self.output.is_closed() {
                break;
            }
            // 被取消的运行不再自动续跑：引擎的 follow_up 与运行期间已入队的
            // 完成都属于被停止的工作，只结算通知持久化，不创建新运行、
            // 不发起 Provider 请求；内存补齐留给下一次显式输入。
            if was_cancelled {
                self.settle_stopped_completions().await;
                break;
            }
            start = if follow_up {
                RunStart::PersistedTaskNotification
            } else if collected_after_run {
                RunStart::PendingTaskNotification
            } else {
                break;
            };
        }
        self.state.set_current_run(None);
    }

    async fn process_run_inner(
        &mut self,
        start: RunStart,
        run_id: String,
        thread_id: String,
    ) -> (bool, bool, bool) {
        tracing::info!("agent run started");
        let requires_internal_input = matches!(start, RunStart::PendingTaskNotification);
        if let Err(error) = history::persist_initial_message(
            &self.thread_id,
            self.messages.last().cloned(),
            start,
            self.host.as_ref(),
        )
        .await
        {
            self.messages.pop();
            self.state.set_lifecycle(AgentLifecycle::Closing);
            self.send_event(RuntimeToServerEvent::error(error.to_string()))
                .await;
            return (false, true, false);
        }

        self.send_event(RuntimeToServerEvent::RunStarted).await;
        if !self.ensure_mcp_initialized().await {
            self.cancelled.store(true, Ordering::Relaxed);
            self.send_event(RuntimeToServerEvent::RunFinished).await;
            return (false, false, true);
        }
        let tool_registry = self.tool_registry_snapshot();

        // 创建 engine -> runtime 的内部通信通道。
        let (engine_tx, engine_rx) = mpsc::channel::<EngineToRuntimeEvent>(256);
        let active_profile = self.active_profile();
        let active_profile_handle = Arc::clone(&self.active_profile);
        let tool_pause_resolver = self.query_engine.tool_pause_resolver();

        // 启动共享执行事件接收端：负责增量持久化和转发到输出。
        let processor = self
            .spawn_run_sink(
                engine_rx,
                active_profile,
                Arc::clone(&active_profile_handle),
                tool_pause_resolver,
                Some(run_id.clone()),
            )
            .await;

        let (follow_up, failed) = {
            let subagent_registry = self.capabilities.subagent_registry();
            let skill_registry = self.capabilities.skill_registry();
            let run_settings = self.settings.clone();
            let run_settings = Arc::new(run_settings);
            // 查询期间把消息缓冲移入局部变量：查询 future 只借用局部，
            // 运行中的命令处理（配置即时应用、ack 确认）得以继续访问 self。
            let mut messages = std::mem::take(&mut self.messages);
            let ctx = QueryContext {
                messages: &mut messages,
                settings: Arc::clone(&run_settings),
                llm_client: self.llm_client.clone(),
                tool_registry: Arc::clone(&tool_registry),
                active_profile: Arc::clone(&active_profile_handle),
                runtime_context: Some(Arc::new(ToolRuntimeContext {
                    thread_id: self.thread_id.clone(),
                    run_id: Some(run_id.clone()),
                    thread_type: "main".to_string(),
                    agent_label: None,
                    thread_dir: self.thread_dir.clone(),
                    llm_context_version: Arc::clone(&self.llm_context_version),
                    agent_depth: 0,
                    task_id: None,
                    owner_thread_id: self.thread_id.clone(),
                    agent_registry: Arc::clone(&subagent_registry),
                    skill_registry: Arc::clone(&skill_registry),
                    task_manager: Some(self.task_supervisor.task_manager()),
                    task_supervisor: Some(Arc::clone(&self.task_supervisor)),
                    project: self.project.clone(),
                })),
                requires_internal_input,
            };

            // 查询 future（连带 ctx）限制在内层块：块尾析构释放对局部
            // messages 的借用，之后才能把它移回 self。
            let query_result = {
                let query =
                    self.query_engine
                        .run_query(ctx, engine_tx, Arc::clone(&self.cancelled));
                tokio::pin!(query);
                let mut query_result = None;

                loop {
                    tokio::select! {
                        result = &mut query => {
                            query_result = Some(result);
                            break;
                        }
                        command = self.cmd_rx.recv() => {
                            let Some(command) = command else { break };
                            match command {
                                // 配置命令直接按不相交字段借用应用，运行中即时生效；
                                // 其余命令只访问共享句柄。
                                AgentCommand::SetModel {
                                    provider,
                                    model,
                                    thinking_effort,
                                    ack,
                                } => {
                                    let result = active_run::apply_model_selection(
                                        &mut self.settings,
                                        &mut self.llm_client,
                                        &self.project,
                                        Some(&self.thread_id),
                                        active_run::ModelSelection {
                                            provider: &provider,
                                            model: &model,
                                            thinking_effort,
                                        },
                                        active_run::RuntimeSinks {
                                            output: &self.output,
                                            host: self.host.as_ref(),
                                            usage_state: &self.thread_usage,
                                        },
                                    )
                                    .await;
                                    if result.is_ok() {
                                        self.publish_config();
                                    }
                                    let _ = ack.send(result);
                                }
                                AgentCommand::SetThinkingEffort { effort, ack } => {
                                    let result = active_run::apply_thinking_effort(
                                        &mut self.settings,
                                        &self.project,
                                        Some(&self.thread_id),
                                        effort,
                                        &self.output,
                                        self.host.as_ref(),
                                    )
                                    .await;
                                    if result.is_ok() {
                                        self.publish_config();
                                    }
                                    let _ = ack.send(result);
                                }
                                AgentCommand::ToggleActiveProfile { ack } => {
                                    let mut active_profile = *self
                                        .active_profile
                                        .read()
                                        .expect("active profile lock poisoned");
                                    active_run::toggle_active_profile(
                                        &mut active_profile,
                                        &mut self.settings,
                                        &self.capabilities,
                                        &self.output,
                                    )
                                    .await;
                                    *self
                                        .active_profile
                                        .write()
                                        .expect("active profile lock poisoned") = active_profile;
                                    self.publish_config();
                                    let _ = ack.send(Ok(()));
                                }
                                AgentCommand::SetActiveProfile { profile, ack } => {
                                    if profile == ActiveProfile::Plan {
                                        let _ = ack.send(Err(CoreError::new(
                                            "Cannot switch to the planning profile while a run is active",
                                        )));
                                    } else {
                                        *self
                                            .active_profile
                                            .write()
                                            .expect("active profile lock poisoned") = profile;
                                        active_run::rebuild_system_prompt(
                                            &mut self.settings,
                                            &self.capabilities,
                                            profile,
                                        );
                                        self.publish_config();
                                        let _ = self
                                            .output
                                            .send_event(RuntimeToServerEvent::ActiveProfileChanged(
                                                profile,
                                            ))
                                            .await;
                                        let _ = ack.send(Ok(()));
                                    }
                                }
                                other => {
                                    if !self.handle_active_command(other).await {
                                        break;
                                    }
                                }
                            }
                        }
                        Some(completion) = self.task_completion_rx.recv() => {
                            self.handle_run_completion(completion).await;
                            tokio::task::yield_now().await;
                            while let Ok(completion) = self.task_completion_rx.try_recv() {
                                self.handle_run_completion(completion).await;
                            }
                        }
                        () = self.gate.wait_close_requested() => {
                            self.cancelled.store(true, Ordering::Relaxed);
                            self.query_engine.notify_cancel_waiters();
                            break;
                        }
                        // 运行中输出断开同样触发收尾（closed() 只完成一次，
                        // send 失败置位的标志不能用于禁用本分支）。
                        () = self.output.closed() => {
                            tracing::debug!("agent output consumer disconnected during run");
                            self.cancelled.store(true, Ordering::Relaxed);
                            self.query_engine.notify_cancel_waiters();
                            break;
                        }
                    }
                }
                query_result
            };
            if let Some(result) = &query_result {
                tracing::info!(
                    turns = result.turns,
                    finish_reason = ?result.finish_reason,
                    "query finished"
                );
            }
            self.messages = messages;
            query_result.map_or((false, false), |result| {
                (
                    result.follow_up,
                    matches!(
                        result.finish_reason,
                        omini_provider_api::FinishReason::Error(_)
                    ),
                )
            })
        };

        // 等待事件接收端在 engine_tx drop 后自然退出。
        let persistence_failed = match processor.await {
            Ok(Some(error)) => {
                self.state.set_lifecycle(AgentLifecycle::Closing);
                self.send_event(RuntimeToServerEvent::error(error)).await;
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.state.set_lifecycle(AgentLifecycle::Closing);
                self.send_event(RuntimeToServerEvent::error(error.to_string()))
                    .await;
                true
            }
        };

        let was_cancelled = self.cancelled.load(Ordering::Relaxed);
        self.cancelled.store(false, Ordering::Relaxed);
        self.send_event(RuntimeToServerEvent::RunFinished).await;
        tracing::info!(
            thread_id = %thread_id,
            run_id = %run_id,
            "agent run finished"
        );

        match self.persist_latest_proposed_plan().await {
            Ok(Some(plan)) if !was_cancelled => {
                self.send_event(RuntimeToServerEvent::PlanSubmitted(plan))
                    .await;
            }
            Ok(Some(_plan)) => {
                tracing::info!("PlanSubmitted suppressed: run was cancelled");
            }
            Ok(None) => {}
            Err(error) => {
                self.send_event(RuntimeToServerEvent::error(error)).await;
            }
        }
        (
            follow_up && !persistence_failed,
            failed || persistence_failed,
            was_cancelled,
        )
    }

    /// 批量收集通道中的完成。停止边界外的旧完成也可能滞留通道——新
    /// 运行收尾时的本收集同样不得给予其唤醒资格：停止完成直接结算入箱，
    /// 不进引擎队列，也就不会以 `collected_after_run` 触发额外续跑。
    async fn collect_task_completions(&mut self) -> bool {
        tokio::task::yield_now().await;
        let mut collected = false;
        let mut stopped = Vec::new();
        while let Ok(completion) = self.task_completion_rx.try_recv() {
            if self
                .task_supervisor
                .completion_is_stopped(&completion.task_id)
            {
                stopped.push(completion);
            } else {
                self.query_engine.enqueue_task_completion(completion);
                collected = true;
            }
        }
        if !stopped.is_empty() {
            self.persist_stopped_completions(stopped).await;
        }
        collected
    }

    async fn ensure_mcp_initialized(&mut self) -> bool {
        if self.state.lifecycle() != AgentLifecycle::Active {
            return false;
        }
        if self.mcp_initialized {
            return true;
        }
        if let Some(mut initialization) = self.mcp_initialization.take() {
            tokio::select! {
                _ = &mut initialization => {}
                () = self.gate.wait_close_requested() => {
                    initialization.abort();
                    let _ = initialization.await;
                    return false;
                }
                () = self.output.closed() => {
                    initialization.abort();
                    let _ = initialization.await;
                    self.state.set_lifecycle(AgentLifecycle::Closing);
                    return false;
                }
            }
        }
        self.mcp_initialized = true;
        self.state.lifecycle() == AgentLifecycle::Active
    }

    pub fn tool_registry_snapshot(&self) -> Arc<ToolRegistry> {
        let mut registry = self.tool_registry.as_ref().clone();
        self.mcp_manager.register_available_tools(&mut registry);
        Arc::new(registry)
    }

    fn start_mcp_initialization(&mut self) {
        if self.mcp_manager.is_empty() {
            return;
        }

        let manager = Arc::clone(&self.mcp_manager);
        let output = self.output.clone();
        self.mcp_initialization = Some(tokio::spawn(
            async move {
                tracing::debug!("starting background mcp initialization");
                for warning in manager.initialize().await {
                    let _ = output
                        .send_event(RuntimeToServerEvent::warning(warning))
                        .await;
                }
                tracing::debug!("background mcp initialization finished");
            }
            .instrument(tracing::debug_span!("mcp_initialization")),
        ));
    }

    /// 发送事件到输出；发送失败视为输出断开，由主循环收尾。
    pub(crate) async fn send_event(&self, event: RuntimeToServerEvent) {
        let _ = self.output.send_event(event).await;
    }
}
