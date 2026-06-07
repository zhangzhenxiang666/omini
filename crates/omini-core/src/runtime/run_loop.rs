use super::service::{AgentRuntime, RunStart, initial_display_message};
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
                            ServerToRuntimeEvent::SendMessage { draft, client_echo_id } => {
                                tracing::debug!(request_kind = "send_message", client_echo_id = ?client_echo_id, "runtime request received");
                                self.submit_user_message(draft, client_echo_id).await;
                            }
                            ServerToRuntimeEvent::CompactContext { instructions } => {
                                tracing::debug!(request_kind = "compact_context", has_instructions = instructions.is_some(), "runtime request received");
                                self.compact_context(instructions.as_deref()).await;
                            }
                            ServerToRuntimeEvent::SetThinkingEffort(effort) => {
                                tracing::debug!(request_kind = "set_thinking_effort", thinking_effort = ?effort, "runtime request received");
                                active_run::apply_thinking_effort(
                                    &mut self.settings,
                                    &self.project,
                                    self.session_id.as_deref(),
                                    effort,
                                    &self.event_tx,
                                    &self.persistence_tx,
                                )
                                .await;
                            }
                            ServerToRuntimeEvent::SetThinkingDisplay { show } => {
                                tracing::debug!(request_kind = "set_thinking_display", show, "runtime request received");
                                active_run::set_thinking_display(&self.project, show, &self.event_tx)
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
                            ServerToRuntimeEvent::InterveneMessage { draft, .. } => {
                                let _ = draft;
                                tracing::debug!(request_kind = "intervene_message", "runtime request rejected because no run is active");
                                self.send_event(RuntimeToServerEvent::error(
                                    "Cannot intervene because no run is active".to_string(),
                                ))
                                .await;
                            }
                            ServerToRuntimeEvent::CancelRun => {
                                tracing::debug!(request_kind = "cancel_run", "runtime request received");
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.cancel_current_run();
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
                            ServerToRuntimeEvent::HydrateSessionSnapshot { snapshot } => {
                                tracing::debug!(
                                    request_kind = "hydrate_session_snapshot",
                                    session_id = %snapshot.session_id,
                                    message_count = snapshot.messages.len(),
                                    "runtime request received"
                                );
                                self.hydrate_session_snapshot(snapshot).await;
                            }
                            ServerToRuntimeEvent::CloseRuntime => {
                                tracing::debug!(request_kind = "close_runtime", "runtime request received");
                                break;
                            }
                            ServerToRuntimeEvent::SubagentRegistryChanged => {
                                tracing::debug!(request_kind = "subagent_registry_changed", "runtime request received");
                                self.reload_subagent_registry();
                            }
                            ServerToRuntimeEvent::ResolveToolPause { .. } => {
                                tracing::debug!(request_kind = "resolve_tool_pause", "stale tool pause resolution ignored");
                                // 过期的权限响应可能在其他客户端已处理暂停、运行继续后抵达。
                            }
                            ServerToRuntimeEvent::ResolvePlanApproval { plan_id, action } => {
                                tracing::debug!(request_kind = "resolve_plan_approval", plan_id = %plan_id, action = ?action, "runtime request received");
                                self.resolve_plan_approval(&plan_id, action).await;
                            }
                        }
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
            self.session_id.as_deref(),
            active_run::ModelSelection {
                provider,
                model,
                thinking_effort,
            },
            active_run::RuntimeSinks {
                event_tx: &self.event_tx,
                persistence_tx: &self.persistence_tx,
                usage_state: &self.session_usage,
            },
        )
        .await;
    }

    pub(super) async fn toggle_active_profile(&mut self) {
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

    pub(super) fn rebuild_system_prompt(&mut self) {
        let active_profile = self.active_profile();
        active_run::rebuild_system_prompt(&mut self.settings, &self.capabilities, active_profile);
    }

    /// 接收一条用户消息，先回显给 UI，再启动运行。
    pub(super) async fn submit_user_message(
        &mut self,
        draft: UserDraft,
        client_echo_id: Option<String>,
    ) {
        tracing::debug!(client_echo_id = ?client_echo_id, "submitting user message");
        let submission = match draft.into_submission() {
            Ok(submission) => submission,
            Err(error) => {
                tracing::warn!(error = %error, "invalid user message submission");
                self.send_event(RuntimeToServerEvent::error(error)).await;
                return;
            }
        };
        let history_item = submission.clone().history_item();
        self.messages.push(submission.llm_message);
        self.send_event(RuntimeToServerEvent::UserMessageInjected {
            item: history_item,
            client_echo_id,
        })
        .await;
        if let Some(display_message) = submission.display_message {
            self.process_run(RunStart::SplitDisplayMessage { display_message })
                .await;
        } else {
            self.process_run(RunStart::UserMessage).await;
        }
    }

    /// 处理一次完整的用户请求，可能包含多轮 LLM 调用。
    pub(super) async fn process_run(&mut self, start: RunStart) {
        let run_id = Uuid::new_v4().to_string();
        if self.session_id.is_none() {
            self.create_session(initial_display_message(&start)).await;
        } else {
            // 已有 session，更新 updated_at 时间戳。
            let id = self.session_id.as_ref().expect("session_id should exist");
            let _ = self
                .persistence_tx
                .send(RuntimePersistenceEvent::UpdateSessionUpdatedAt {
                    session_id: id.clone(),
                })
                .await;
        }

        let session_id = self
            .session_id
            .clone()
            .expect("session must exist before processing run");
        let run_span = tracing::info_span!(
            "run",
            session_id = %session_id,
            run_id = %run_id,
            start_kind = start.kind(),
            provider = %self.settings.active_provider,
            model = %self.settings.model,
            thinking_effort = ?self.settings.thinking_effort,
            max_turns = ?self.settings.max_turns,
        );
        self.process_run_inner(start, run_id, session_id)
            .instrument(run_span)
            .await;
    }

    async fn process_run_inner(&mut self, start: RunStart, run_id: String, session_id: String) {
        tracing::info!("agent run started");
        history::persist_initial_user_message(
            self.session_id.as_deref(),
            self.session_dir.as_ref(),
            self.messages.last().cloned(),
            start,
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
            .spawn_event_processor(
                engine_rx,
                active_profile,
                Arc::clone(&active_profile_handle),
                tool_pause_resolver,
            )
            .await;

        {
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
                active_profile,
                runtime_context: Some(Arc::new(ToolRuntimeContext {
                    session_id: self
                        .session_id
                        .clone()
                        .expect("session must exist before query"),
                    run_id: Some(run_id.clone()),
                    session_type: "main".to_string(),
                    agent_label: None,
                    session_dir: self
                        .session_dir
                        .clone()
                        .expect("session dir must exist before query"),
                    subagent_registry: Arc::clone(&subagent_registry),
                    skill_registry: Arc::clone(&skill_registry),
                    subagent_runner: Some(Arc::clone(&self.subagent_runner)),
                    project: self.project.clone(),
                })),
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
                            ServerToRuntimeEvent::CancelRun => {
                                tracing::debug!("active run cancellation requested");
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.cancel_current_run();
                            }
                            ServerToRuntimeEvent::ResolveToolPause { tool_use_id, response } => {
                                tracing::debug!(tool_use_id = %tool_use_id, response = ?response, "resolving tool pause");
                                if let Err(e) = self
                                    .query_engine
                                    .resolve_tool_pause(&tool_use_id, response)
                                {
                                    tracing::warn!(tool_use_id = %tool_use_id, error = %e, "failed to resolve tool pause");
                                    let _ =
                                        event_tx.send(RuntimeToServerEvent::error(e.to_string())).await;
                                }
                            }
                            ServerToRuntimeEvent::InterveneMessage { draft, client_echo_id } => {
                                tracing::debug!(client_echo_id = ?client_echo_id, "active run intervention received");
                                let submission = match draft.into_submission() {
                                    Ok(submission) => submission,
                                    Err(error) => {
                                        tracing::warn!(error = %error, "invalid intervention submission");
                                        let _ = event_tx.send(RuntimeToServerEvent::error(error)).await;
                                        continue;
                                    }
                                };
                                self.query_engine
                                    .enqueue_user_message(submission.llm_message, client_echo_id);
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
                                    self.session_id.as_deref(),
                                    effort,
                                    &event_tx,
                                    &self.persistence_tx,
                                )
                                .await;
                            }
                            ServerToRuntimeEvent::SetThinkingDisplay { show } => {
                                tracing::debug!(show, "active run thinking display update");
                                active_run::set_thinking_display(&self.project, show, &event_tx)
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
                                    self.session_id.as_deref(),
                                    active_run::ModelSelection {
                                        provider: &provider,
                                        model: &model,
                                        thinking_effort,
                                    },
                                    active_run::RuntimeSinks {
                                        event_tx: &event_tx,
                                        persistence_tx: &self.persistence_tx,
                                        usage_state: &self.session_usage,
                                    },
                                )
                                .await;
                            }
                            ServerToRuntimeEvent::SendMessage { .. }
                            | ServerToRuntimeEvent::CompactContext { .. }
                            | ServerToRuntimeEvent::HydrateSessionSnapshot { .. }
                            | ServerToRuntimeEvent::CloseRuntime
                            => {
                                tracing::debug!("active run request rejected");
                                active_run::reject_request(&event_tx).await;
                            }
                        }
                    }
                    else => break,
                }
            }
            if let Some(result) = query_result {
                tracing::info!(
                    turns = result.turns,
                    finish_reason = ?result.finish_reason,
                    had_tool_use = result.had_tool_use,
                    "query finished"
                );
            }
        }

        // 等待事件处理器在 engine_tx drop 后自然退出。
        let _ = processor.await;

        self.cancelled.store(false, Ordering::Relaxed);
        self.send_event(RuntimeToServerEvent::RunFinished).await;
        tracing::info!(
            session_id = %session_id,
            run_id = %run_id,
            "agent run finished"
        );

        match self.persist_latest_proposed_plan().await {
            Ok(Some(plan)) => {
                self.send_event(RuntimeToServerEvent::PlanSubmitted(plan))
                    .await;
            }
            Ok(None) => {}
            Err(error) => {
                self.send_event(RuntimeToServerEvent::error(error)).await;
            }
        }
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

    pub(super) fn tool_registry_snapshot(&self) -> Arc<ToolRegistry> {
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
    pub(crate) async fn send_event(&self, event: RuntimeToServerEvent) {
        let _ = self.event_tx.send(event).await;
    }
}
