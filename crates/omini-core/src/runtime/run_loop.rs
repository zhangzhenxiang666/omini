use super::service::{AgentRuntime, RunStart};
use super::*;
use tracing::Instrument;

impl AgentRuntime {
    /// 启动运行时，返回 JoinHandle。
    pub fn run(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tracing::debug!("agent runtime task started");
            self.start_mcp_initialization();
            loop {
                tokio::select! {
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            ServerToRuntimeEvent::SendMessage { message } => {
                                tracing::debug!(request_kind = "send_message", "runtime request received");
                                self.submit_user_message(message).await;
                            }
                            ServerToRuntimeEvent::CompactContext { instructions } => {
                                tracing::debug!(request_kind = "compact_context", has_instructions = instructions.is_some(), "runtime request received");
                                self.handle_compact_context(instructions).await;
                            }
                            ServerToRuntimeEvent::SetThinkingEffort(effort) => {
                                tracing::debug!(request_kind = "set_thinking_effort", thinking_effort = ?effort, "runtime request received");
                                active_run::apply_thinking_effort(
                                    &mut self.settings,
                                    &self.project,
                                    Some(&self.thread_id),
                                    effort,
                                    &self.event_tx,
                                    &self.persistence_tx,
                                )
                                .await;
                            }
                            ServerToRuntimeEvent::ToggleActiveProfile => {
                                tracing::debug!(request_kind = "toggle_active_profile", "runtime request received");
                                self.toggle_active_profile().await;
                            }
                            ServerToRuntimeEvent::SetActiveProfile(profile) => {
                                tracing::debug!(request_kind = "set_active_profile", active_profile = ?profile, "runtime request received");
                                self.set_active_profile(profile);
                                self.send_event(RuntimeToServerEvent::ActiveProfileChanged(
                                    self.active_profile(),
                                ))
                                .await;
                            }
                            ServerToRuntimeEvent::InterveneMessage { run_id: None, .. } => {
                                tracing::debug!(request_kind = "intervene_message", "runtime request rejected because no run is active");
                                self.send_event(RuntimeToServerEvent::error(
                                    "Cannot intervene because no run is active".to_string(),
                                ))
                                .await;
                            }
                            ServerToRuntimeEvent::InterveneMessage {
                                run_id: Some(run_id),
                                message,
                            } => {
                                if let Err(error) = self
                                    .task_supervisor
                                    .intervene_agent_run(&run_id, message)
                                {
                                    let _ = self.send_event(RuntimeToServerEvent::error(error)).await;
                                }
                            }
                            ServerToRuntimeEvent::CancelRun { run_id: None } => {
                                tracing::debug!(request_kind = "cancel_run", "runtime request received");
                                self.task_supervisor.cancel_all().await;
                            }
                            ServerToRuntimeEvent::CancelRun {
                                run_id: Some(run_id),
                            } => {
                                self.task_supervisor.cancel_task(&run_id).await;
                            }
                            ServerToRuntimeEvent::ModelSelected { provider, model, thinking_effort } => {
                                tracing::debug!(
                                    request_kind = "model_selected",
                                    provider = %provider,
                                    model = %model,
                                    thinking_effort = ?thinking_effort,
                                    "runtime request received"
                                );
                                self.switch_model(&provider, &model, thinking_effort).await;
                            }
                            ServerToRuntimeEvent::CloseRuntime => {
                                tracing::debug!(request_kind = "close_runtime", "runtime request received");
                                self.task_supervisor.cancel_all().await;
                                self.task_supervisor.wait_until_idle().await;
                                break;
                            }
                            ServerToRuntimeEvent::SubagentRegistryChanged => {
                                tracing::debug!(request_kind = "subagent_registry_changed", "runtime request received");
                                self.reload_subagent_registry();
                            }
                            ServerToRuntimeEvent::ResolveToolPause { tool_use_id, response } => {
                                if let Err(error) = self.query_engine.resolve_tool_pause(&tool_use_id, response) {
                                    tracing::debug!(tool_use_id, %error, "stale tool pause resolution ignored");
                                }
                            }
                            ServerToRuntimeEvent::ResolvePlanApproval { plan_id, action } => {
                                tracing::debug!(request_kind = "resolve_plan_approval", plan_id = %plan_id, action = ?action, "runtime request received");
                                self.resolve_plan_approval(&plan_id, action).await;
                            }
                        }
                    }
                    Some(completion) = self.task_completion_rx.recv() => {
                        self.query_engine.enqueue_task_completion(completion);
                        self.collect_task_completions().await;
                        self.process_run(RunStart::PendingTaskNotification).await;
                    }
                    else => break,
                }
            }
            tracing::debug!("agent runtime task stopped");
        }
        .in_current_span())
    }

    /// 切换模型 / 提供商，在 /model 交互完成后回调。
    async fn switch_model(
        &mut self,
        provider: &str,
        model: &str,
        thinking_effort: Option<ThinkingEffort>,
    ) {
        active_run::apply_model_selection(
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
                event_tx: &self.event_tx,
                persistence_tx: &self.persistence_tx,
                usage_state: &self.thread_usage,
            },
        )
        .await;
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

    /// 接收一条用户消息，追加进历史并启动运行。
    /// 展示行入库与 echo 已由 server 在接收时完成，这里只管 LLM 上下文。
    pub async fn submit_user_message(&mut self, message: omini_model::message::Message) {
        self.messages.push(message);
        self.process_run(RunStart::UserInput).await;
    }

    /// 处理一次完整的用户请求，可能包含多轮 LLM 调用。
    ///
    /// `AgentRuntime` 始终绑定一个已存在的 thread，所以这里只需刷新 `updated_at`
    /// 然后进入 query loop；不再生成 UUID、建目录或写 title —— 这些都交由 server。
    pub async fn process_run(&mut self, mut start: RunStart) {
        loop {
            self.collect_task_completions().await;
            let run_id = Uuid::new_v4().to_string();
            let created_at = chrono::Utc::now();
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
            let _ = self
                .persistence_tx
                .send(RuntimePersistenceEvent::CreateAgentRun {
                    run: Box::new(run_snapshot.clone()),
                })
                .await;
            let _ = self
                .event_tx
                .send(RuntimeToServerEvent::AgentRunChanged(run_snapshot.clone()))
                .await;
            let _ = self
                .persistence_tx
                .send(RuntimePersistenceEvent::UpdateThreadUpdatedAt {
                    thread_id: self.thread_id.clone(),
                })
                .await;

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
            let (follow_up, failed) = self
                .process_run_inner(start, run_id.clone(), thread_id)
                .instrument(run_span)
                .await;
            let status = if self.cancelled.load(Ordering::Relaxed) {
                omini_domain::agent_run::AgentRunStatus::Cancelled
            } else if failed {
                omini_domain::agent_run::AgentRunStatus::Failed
            } else {
                omini_domain::agent_run::AgentRunStatus::Completed
            };
            run_snapshot.status = status;
            run_snapshot.finished_at = Some(chrono::Utc::now());
            let _ = self
                .persistence_tx
                .send(RuntimePersistenceEvent::UpdateAgentRun {
                    run_id,
                    status,
                    started_at: None,
                    finished_at: run_snapshot.finished_at,
                    add_tokens: 0,
                })
                .await;
            let _ = self
                .event_tx
                .send(RuntimeToServerEvent::AgentRunChanged(run_snapshot))
                .await;
            let collected_after_run = self.collect_task_completions().await;
            start = if follow_up {
                RunStart::PersistedTaskNotification
            } else if collected_after_run {
                RunStart::PendingTaskNotification
            } else {
                break;
            };
        }
    }

    async fn process_run_inner(
        &mut self,
        start: RunStart,
        run_id: String,
        thread_id: String,
    ) -> (bool, bool) {
        tracing::info!("agent run started");
        let requires_internal_input = matches!(start, RunStart::PendingTaskNotification);
        let model = self.settings.active_model();
        history::persist_initial_user_message(
            &self.thread_id,
            self.messages.last().cloned(),
            start,
            &format!("{}/{}", model.provider_id, model.model_id),
            &self.persistence_tx,
        )
        .await;

        self.send_event(RuntimeToServerEvent::RunStarted).await;
        self.ensure_mcp_initialized().await;
        let tool_registry = self.tool_registry_snapshot();

        // 创建 engine -> runtime 的内部通信通道。
        let (engine_tx, engine_rx) = mpsc::channel::<EngineToRuntimeEvent>(256);
        let active_profile = self.active_profile();
        let active_profile_handle = Arc::clone(&self.active_profile);
        let tool_pause_resolver = self.query_engine.tool_pause_resolver();

        // 启动事件处理器独立 task，负责增量持久化和转发到 server。
        let processor = self
            .spawn_event_processor_for_run(
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
            // 引擎直接在当前 task 运行，让 &mut self.messages 保持零拷贝。
            let ctx = QueryContext {
                messages: &mut self.messages,
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

            let event_tx = self.event_tx.clone();
            let query = self
                .query_engine
                .run_query(ctx, engine_tx, Arc::clone(&self.cancelled));
            tokio::pin!(query);
            let mut query_result = None;

            loop {
                tokio::select! {
                    result = &mut query => {
                        query_result = Some(result);
                        break;
                    }
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            ServerToRuntimeEvent::CancelRun { run_id: None } => {
                                tracing::debug!("active run cancellation requested");
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.notify_cancel_waiters();
                                self.task_supervisor.cancel_all().await;
                            }
                            ServerToRuntimeEvent::CancelRun {
                                run_id: Some(run_id),
                            } => {
                                self.task_supervisor.cancel_task(&run_id).await;
                            }
                            ServerToRuntimeEvent::ResolveToolPause { tool_use_id, response } => {
                                tracing::debug!(tool_use_id = %tool_use_id, response = ?response, "resolving tool pause");
                                let permission_response = matches!(
                                    response,
                                    omini_runtime_contract::thread_domain::ToolPauseResponse::Permission { .. }
                                );
                                if let Err(e) = self
                                    .query_engine
                                    .resolve_tool_pause(&tool_use_id, response)
                                {
                                    tracing::warn!(tool_use_id = %tool_use_id, error = %e, "failed to resolve tool pause");
                                    let _ =
                                        event_tx.send(RuntimeToServerEvent::error(e.to_string())).await;
                                } else if permission_response {
                                    let _ = self.persistence_tx
                                        .send(RuntimePersistenceEvent::UpdateAgentRun {
                                            run_id: run_id.clone(),
                                            status: omini_domain::agent_run::AgentRunStatus::Running,
                                            started_at: None,
                                            finished_at: None,
                                            add_tokens: 0,
                                        })
                                        .await;
                                }
                            }
                            ServerToRuntimeEvent::InterveneMessage {
                                run_id: None,
                                message,
                            } => {
                                tracing::debug!(request_kind = "intervene_message", "active run intervention received");
                                self.query_engine.enqueue_user_message(message);
                            }
                            ServerToRuntimeEvent::InterveneMessage {
                                run_id: Some(run_id),
                                message,
                            } => {
                                if let Err(error) = self
                                    .task_supervisor
                                    .intervene_agent_run(&run_id, message)
                                {
                                    let _ = event_tx.send(RuntimeToServerEvent::error(error)).await;
                                }
                            }
                            ServerToRuntimeEvent::ResolvePlanApproval { plan_id, action } => {
                                tracing::debug!(plan_id = %plan_id, action = ?action, "plan approval resolution rejected during active run");
                                let _ = (plan_id, action);
                                let _ = event_tx
                                    .send(RuntimeToServerEvent::error(
                                        "Cannot resolve plan approval while a run is active".to_string(),
                                    ))
                                    .await;
                            }
                            ServerToRuntimeEvent::SetThinkingEffort(effort) => {
                                tracing::debug!(thinking_effort = ?effort, "active run thinking effort update");
                                active_run::apply_thinking_effort(
                                    &mut self.settings,
                                    &self.project,
                                    Some(&self.thread_id),
                                    effort,
                                    &event_tx,
                                    &self.persistence_tx,
                                )
                                .await;
                            }
                            ServerToRuntimeEvent::ToggleActiveProfile => {
                                tracing::debug!("active run profile toggle requested");
                                let mut active_profile = *active_profile_handle
                                    .read()
                                    .expect("active profile lock poisoned");
                                active_run::toggle_active_profile(
                                    &mut active_profile,
                                    &mut self.settings,
                                    &self.capabilities,
                                    &event_tx,
                                )
                                .await;
                                *active_profile_handle
                                    .write()
                                    .expect("active profile lock poisoned") = active_profile;
                            }
                            ServerToRuntimeEvent::SetActiveProfile(profile) => {
                                tracing::debug!(active_profile = ?profile, "active run profile update requested");
                                if profile == ActiveProfile::Plan {
                                    active_run::reject_request(&event_tx).await;
                                } else {
                                    *active_profile_handle
                                        .write()
                                        .expect("active profile lock poisoned") = profile;
                                    active_run::rebuild_system_prompt(
                                        &mut self.settings,
                                        &self.capabilities,
                                        profile,
                                    );
                                    let _ = event_tx
                                        .send(RuntimeToServerEvent::ActiveProfileChanged(profile))
                                        .await;
                                }
                            }
                            ServerToRuntimeEvent::SubagentRegistryChanged => {
                                active_run::reject_request(&event_tx).await;
                            }
                            ServerToRuntimeEvent::ModelSelected { provider, model, thinking_effort } => {
                                tracing::debug!(
                                    provider = %provider,
                                    model = %model,
                                    thinking_effort = ?thinking_effort,
                                    "active run model update requested"
                                );
                                active_run::apply_model_selection(
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
                                        event_tx: &event_tx,
                                        persistence_tx: &self.persistence_tx,
                                        usage_state: &self.thread_usage,
                                    },
                                )
                                .await;
                            }
                            ServerToRuntimeEvent::SendMessage { .. }
                            | ServerToRuntimeEvent::CompactContext { .. }
                            | ServerToRuntimeEvent::CloseRuntime
                            => {
                                tracing::debug!("active run request rejected");
                                active_run::reject_request(&event_tx).await;
                            }
                        }
                    }
                    Some(completion) = self.task_completion_rx.recv() => {
                        self.query_engine.enqueue_task_completion(completion);
                        tokio::task::yield_now().await;
                        while let Ok(completion) = self.task_completion_rx.try_recv() {
                            self.query_engine.enqueue_task_completion(completion);
                        }
                    }
                    else => break,
                }
            }
            if let Some(result) = &query_result {
                tracing::info!(
                    turns = result.turns,
                    finish_reason = ?result.finish_reason,
                    "query finished"
                );
            }
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

        // 等待事件处理器在 engine_tx drop 后自然退出。
        let _ = processor.await;

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
        (follow_up, failed)
    }

    async fn collect_task_completions(&mut self) -> bool {
        tokio::task::yield_now().await;
        let mut collected = false;
        while let Ok(completion) = self.task_completion_rx.try_recv() {
            self.query_engine.enqueue_task_completion(completion);
            collected = true;
        }
        collected
    }

    async fn ensure_mcp_initialized(&mut self) {
        if self.mcp_initialized {
            return;
        }
        self.mcp_initialized = true;

        if !self.mcp_manager.is_empty() {
            let _ = self.mcp_manager.initialize().await;
        }
    }

    pub fn tool_registry_snapshot(&self) -> Arc<ToolRegistry> {
        let mut registry = self.tool_registry.as_ref().clone();
        self.mcp_manager.register_available_tools(&mut registry);
        Arc::new(registry)
    }

    fn start_mcp_initialization(&self) {
        if self.mcp_manager.is_empty() {
            return;
        }

        let manager = Arc::clone(&self.mcp_manager);
        let event_tx = self.event_tx.clone();
        tokio::spawn(
            async move {
                tracing::debug!("starting background mcp initialization");
                for warning in manager.initialize().await {
                    let _ = event_tx.send(RuntimeToServerEvent::warning(warning)).await;
                }
                tracing::debug!("background mcp initialization finished");
            }
            .instrument(tracing::debug_span!("mcp_initialization")),
        );
    }

    /// 发送事件到 server/facade，忽略 send 失败。
    pub async fn send_event(&self, event: RuntimeToServerEvent) {
        let _ = self.event_tx.send(event).await;
    }
}
