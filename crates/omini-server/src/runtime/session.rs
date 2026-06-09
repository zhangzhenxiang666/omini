use super::*;
use omini_domain::events::{Notification, SessionUsageSnapshot};
use tracing::Instrument;

/// projection 和 replay buffer。HTTP 路由拿到的 `RuntimeSession` 不直接操作 core 的内部
/// loop，而是通过这个类型做 daemon 级的持久化、重连补发和多客户端控制权协调。
pub(crate) struct RuntimeSession {
    // 单个 daemon session 对应的 core facade；HTTP/controller 校验后的用户动作从这里进入 core。
    pub(crate) core: AgentCoreSession,
    // daemon 会话 ID，同时也是数据库、项目 session 目录和 WebSocket 路由使用的稳定 ID。
    pub(super) session_id: String,
    // 当前项目的目录句柄，用于加载 session snapshot、subagent 历史和 block 文件。
    project: ProjectDir,
    // 创建 runtime 时的项目配置快照；server 用它补充 snapshot/status 中的只读信息。
    settings: Settings,
    // session 元数据、消息、usage 和 core persistence event 的 SQLite 存储。
    db: Arc<Database>,
    // core runtime 事件经过本地 seq 编号后的广播流，WebSocket 订阅和 replay 去重都用它。
    runtime_event_tx: broadcast::Sender<SequencedRuntimeEvent>,
    // server 本地产生的协议事件入口，例如 session title 变更；fanout 会统一编号和广播。
    server_event_inbox_tx: mpsc::UnboundedSender<RuntimeEvent>,
    // 当前连接的 client 集合和 controller 归属；HTTP mutation 会用它做控制权检查。
    pub(super) presence: Mutex<ClientPresence>,
    // 尚未 resolve 的 tool pause id 集合；resolve API 用它保证幂等并防止重复点击。
    pending_tool_pauses: Arc<Mutex<HashSet<String>>>,
    // 从 runtime 事件流派生的轻量状态投影，供 session status API 快速读取。
    status_projection: Arc<Mutex<RuntimeStatusProjection>>,
    // 尚未被 snapshot 或持久化覆盖的运行中事件尾部，用于 WebSocket 重连补发。
    replay_buffer: Arc<Mutex<RuntimeReplayBuffer>>,
    // controller 变化广播流；WebSocket 连接用它同步观察者/控制者状态。
    controller_tx: broadcast::Sender<Option<String>>,
    // persisted snapshot 到 core runtime 的加载闸门，保证并发请求只触发一次 load。
    loaded: RuntimeLoadGate,
    // core 持久化事件任务：落 SQLite，成功后裁剪 replay buffer 中已持久化的尾部事件。
    _persistence_handle: JoinHandle<()>,
    // core runtime 事件任务：分配本地 seq，更新 replay/status，再广播给 WebSocket 层。
    _runtime_event_handle: JoinHandle<()>,
    // tool pause 跟踪任务：监听 runtime 事件并维护 pending_tool_pauses 集合。
    _tool_pause_handle: JoinHandle<()>,
}

impl RuntimeSession {
    pub(super) fn spawn(
        settings: Settings,
        project: ProjectDir,
        session_id: String,
        db: Arc<Database>,
        active_profile: ActiveProfile,
    ) -> Self {
        let (controller_tx, _) = broadcast::channel(32);
        let (runtime_event_tx, _) = broadcast::channel(512);
        let (server_event_inbox_tx, mut server_event_inbox_rx) = mpsc::unbounded_channel();
        let core = AgentCoreSession::spawn_for_session_with_active_profile(
            settings.clone(),
            project.clone(),
            session_id.clone(),
            active_profile,
        );
        let mut persistence_rx = core.subscribe_persistence();
        let mut tool_pause_rx = core.subscribe();
        let mut runtime_event_rx = core.subscribe();
        let replay_buffer = Arc::new(Mutex::new(RuntimeReplayBuffer::default()));
        let status_projection = Arc::new(Mutex::new(RuntimeStatusProjection::with_active_profile(
            active_profile,
        )));
        let persistence_db = Arc::clone(&db);
        let persisted_replay_buffer = Arc::clone(&replay_buffer);
        let replay_session_id = session_id.clone();
        // core 发出的持久化事件先落 SQLite，成功后再裁剪 replay，避免重连时漏掉未落盘内容。
        let persistence_handle = tokio::spawn(
            async move {
                loop {
                    match persistence_rx.recv().await {
                        Ok(event) => {
                            let persisted_event = event.clone();
                            if let Err(error) = persistence_db.apply_persistence_event(event).await
                            {
                                tracing::error!(error = %error, "runtime persistence event failed");
                            } else {
                                persisted_replay_buffer
                                    .lock()
                                    .expect("replay buffer lock poisoned")
                                    .record_persistence(&replay_session_id, &persisted_event);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "runtime persistence event stream lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
            .instrument(tracing::debug_span!(
                "session",
                session_id = %session_id,
                task_kind = "persistence_fanout"
            )),
        );
        let runtime_event_fanout_tx = runtime_event_tx.clone();
        let runtime_replay_buffer = Arc::clone(&replay_buffer);
        let runtime_status_projection = Arc::clone(&status_projection);
        // runtime 事件加上本地 seq 后再广播，WebSocket 层用 seq 处理 replay/订阅交叠。
        let runtime_event_handle = tokio::spawn(
            async move {
                let mut next_seq = 1u64;
                loop {
                    tokio::select! {
                        event = runtime_event_rx.recv() => {
                            match event {
                                Ok(event) => {
                                    let Some(event) = runtime_event_from_core_with_fallback(event) else {
                                        continue;
                                    };
                                    broadcast_sequenced_runtime_event(
                                        event,
                                        &mut next_seq,
                                        &runtime_replay_buffer,
                                        &runtime_status_projection,
                                        &runtime_event_fanout_tx,
                                    );
                                }
                                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                    tracing::warn!(skipped, "runtime event stream lagged");
                                }
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        }
                        event = server_event_inbox_rx.recv() => {
                            let Some(event) = event else {
                                break;
                            };
                            broadcast_sequenced_runtime_event(
                                event,
                                &mut next_seq,
                                &runtime_replay_buffer,
                                &runtime_status_projection,
                                &runtime_event_fanout_tx,
                            );
                        }
                    }
                }
            }
            .instrument(tracing::debug_span!(
                "session",
                session_id = %session_id,
                task_kind = "runtime_event_fanout"
            )),
        );
        let pending_tool_pauses = Arc::new(Mutex::new(HashSet::new()));
        let pending_tool_pause_events = Arc::clone(&pending_tool_pauses);
        // 工具暂停状态跟随 runtime 事件维护，HTTP resolve 用它做幂等和重复点击保护。
        let tool_pause_handle = tokio::spawn(
            async move {
                loop {
                    match tool_pause_rx.recv().await {
                        Ok(event) => {
                            apply_tool_pause_update(&pending_tool_pause_events, &event);
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "runtime tool pause event stream lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
            .instrument(tracing::debug_span!(
                "session",
                session_id = %session_id,
                task_kind = "tool_pause_watcher"
            )),
        );
        Self {
            core,
            session_id,
            project,
            settings,
            db,
            runtime_event_tx,
            server_event_inbox_tx,
            presence: Mutex::new(ClientPresence::default()),
            pending_tool_pauses,
            status_projection,
            replay_buffer,
            controller_tx,
            loaded: RuntimeLoadGate::default(),
            _persistence_handle: persistence_handle,
            _runtime_event_handle: runtime_event_handle,
            _tool_pause_handle: tool_pause_handle,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<SequencedRuntimeEvent> {
        self.runtime_event_tx.subscribe()
    }

    pub(crate) async fn replay_events(&self) -> Vec<SequencedRuntimeEvent> {
        self.replay_buffer
            .lock()
            .expect("replay buffer lock poisoned")
            .replay()
    }

    pub(crate) fn subscribe_controller(&self) -> broadcast::Receiver<Option<String>> {
        self.controller_tx.subscribe()
    }

    pub(crate) async fn runtime_status(&self) -> protocol::SessionRuntimeStatus {
        let (controller_id, connected_client_count) = {
            let presence = self.presence.lock().expect("presence lock poisoned");
            (presence.controller_id.clone(), presence.clients.len())
        };
        let loaded = self.loaded.is_loaded();
        let skills = self.core.runtime_skills();
        let mcp_servers = self.core.runtime_mcp_servers();
        let subagent_sessions = self.core.runtime_subagents();
        self.status_projection
            .lock()
            .expect("status projection lock poisoned")
            .to_protocol(
                &self.session_id,
                RuntimeStatusSnapshotContext {
                    loaded,
                    controller_id,
                    connected_client_count,
                    skills,
                    mcp_servers,
                    subagent_sessions,
                    now: Utc::now(),
                },
            )
    }

    pub(crate) async fn reload_subagent_registry(&self) -> Result<(), CoreError> {
        self.core.reload_subagent_registry().await
    }

    pub(crate) fn set_thinking_display(
        &self,
        request: protocol::SetThinkingDisplayRequest,
    ) -> Result<(), CoreError> {
        let show = self.save_thinking_display(request.show)?;
        self.broadcast_thinking_display_changed(show)
    }

    pub(crate) fn broadcast_agent_management_updated(
        &self,
        records: Vec<omini_domain::subagents::AgentRecord>,
    ) -> Result<(), CoreError> {
        let event =
            runtime_event_from_internal(RuntimeToServerEvent::AgentManagementUpdated { records })?;
        self.broadcast_server_local_event(event);
        Ok(())
    }

    pub(crate) fn runtime_state(&self) -> protocol::SessionRuntimeState {
        self.status_projection
            .lock()
            .expect("status projection lock poisoned")
            .state()
    }

    pub(crate) fn is_reclaimable(&self) -> bool {
        self.runtime_state() == protocol::SessionRuntimeState::Idle
    }

    pub(crate) async fn rename_session(&self, title: String) -> Result<(), CoreError> {
        self.db
            .update_session_title(&self.session_id, &title)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to rename session", error.to_string())
            })?;
        self.broadcast_session_title_changed(title)?;
        Ok(())
    }

    pub(crate) async fn set_initial_title_from_input(
        &self,
        input: &protocol::UserInput,
    ) -> Result<(), CoreError> {
        let Some(title) = initial_session_title_from_input(input) else {
            return Ok(());
        };
        let updated = self
            .db
            .set_initial_session_title(&self.session_id, &title)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to set initial session title", error.to_string())
            })?;
        if updated {
            self.broadcast_session_title_changed(title)?;
        }
        Ok(())
    }

    fn broadcast_session_title_changed(&self, title: String) -> Result<(), CoreError> {
        let event = runtime_event_from_internal(RuntimeToServerEvent::SessionTitleChanged {
            title: Some(title),
        })?;
        self.broadcast_server_local_event(event);
        Ok(())
    }

    fn save_thinking_display(&self, show: Option<bool>) -> Result<bool, CoreError> {
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        let show = show.unwrap_or(!state.show_thinking_blocks);
        state.show_thinking_blocks = show;
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;
        Ok(show)
    }

    fn broadcast_thinking_display_changed(&self, show: bool) -> Result<(), CoreError> {
        self.broadcast_server_local_event(thinking_display_changed_event(show));
        let notification = runtime_event_from_internal(RuntimeToServerEvent::Notification(
            thinking_display_notification(show),
        ))?;
        self.broadcast_server_local_event(notification);
        Ok(())
    }

    fn broadcast_server_local_event(&self, event: RuntimeEvent) {
        let _ = self.server_event_inbox_tx.send(event);
    }

    pub(crate) async fn ensure_loaded(&self) -> Result<(), CoreError> {
        match self.loaded.begin_load() {
            RuntimeLoadAction::AlreadyLoaded => Ok(()),
            RuntimeLoadAction::Wait(waiter) => match waiter.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(message)) => Err(CoreError::new(message)),
                Err(_) => Err(CoreError::RuntimeLoadInterrupted),
            },
            RuntimeLoadAction::Load => {
                // core 只加载一次持久化 snapshot；后续运行时事件会继续增量更新 core 状态。
                let result = async {
                    let snapshot = self.load_snapshot().await?;
                    self.core.load_session(snapshot).await
                }
                .await;
                self.loaded.finish_load(&result);
                result
            }
        }
    }

    pub(crate) async fn current_snapshot_events(&self) -> Result<Vec<RuntimeEvent>, CoreError> {
        let snapshot = self.load_snapshot().await?;
        // snapshot 即将发给客户端，先让 replay buffer 去掉已包含在 snapshot 里的尾部事件。
        self.replay_buffer
            .lock()
            .expect("replay buffer lock poisoned")
            .record_snapshot(&snapshot);
        let context_window = self.context_window_for_snapshot(&snapshot);
        let active_profile = self
            .status_projection
            .lock()
            .expect("status projection lock poisoned")
            .active_profile();
        session_snapshot_events(snapshot, context_window, active_profile)
    }

    async fn load_snapshot(&self) -> Result<LoadedSession, CoreError> {
        let session = self
            .db
            .get_session(&self.session_id)
            .await
            .map_err(|error| CoreError::persistence("failed to load session", error.to_string()))?
            .ok_or(CoreError::SessionNotFound)?;
        let session_dir = self.project.session(&self.session_id);
        let blocks_dir = session_dir.path().join("blocks");
        let messages = history::load_messages(&self.db, &self.session_id, &blocks_dir).await;
        let subagents =
            history::load_subagents_for_session(&self.db, &self.session_id, &self.project).await;
        let active_profile = self
            .status_projection
            .lock()
            .expect("status projection lock poisoned")
            .active_profile();
        Ok(LoadedSession {
            session_id: session.id,
            provider: session.provider.clone(),
            model: session.model.clone(),
            thinking_effort: effective_session_thinking_effort(
                &self.settings,
                &session.provider,
                &session.model,
                session.thinking_effort.as_deref(),
            ),
            active_profile,
            title: session.title,
            messages,
            subagents,
            usage: SessionUsageSnapshot {
                current_context_tokens: session.current_context_tokens,
                total_tokens: session.total_tokens,
                total_cached_tokens: session.total_cached_tokens,
                context_window: None,
            },
        })
    }

    fn context_window_for_snapshot(&self, snapshot: &LoadedSession) -> Option<u32> {
        self.settings
            .providers
            .get(&snapshot.provider)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| model.id == snapshot.model)
            })
            .map(|model| model.limit)
    }

    pub(crate) async fn register_client_connection(&self, client_id: String) -> Option<String> {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .register(client_id);
        if changed {
            let _ = self.controller_tx.send(controller_id.clone());
        }
        controller_id
    }

    pub(crate) async fn unregister_client_connection(&self, client_id: &str) {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .unregister(client_id);
        if changed {
            let _ = self.controller_tx.send(controller_id);
        }
    }

    pub(crate) async fn claim_controller(&self, client_id: String) -> Option<String> {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .claim(client_id)?;
        if changed {
            let _ = self.controller_tx.send(controller_id.clone());
        }
        controller_id
    }

    pub(crate) async fn takeover_controller(&self, client_id: String) -> Option<String> {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .takeover(client_id)?;
        if changed {
            let _ = self.controller_tx.send(controller_id.clone());
        }
        controller_id
    }

    pub(crate) async fn begin_tool_pause_resolution(
        &self,
        client_id: String,
        tool_use_id: &str,
    ) -> ToolPauseResolutionStart {
        {
            let mut pending = self
                .pending_tool_pauses
                .lock()
                .expect("pending tool pauses lock poisoned");
            // 先移除 pending，保证同一个 tool pause 只有一个请求能进入 core resolve。
            if !pending.remove(tool_use_id) {
                return ToolPauseResolutionStart::AlreadyResolved;
            }
        }

        // 权限响应来自用户操作，应把发起响应的已连接客户端提升为 controller。
        if self.takeover_controller(client_id).await.is_some() {
            ToolPauseResolutionStart::Started
        } else {
            // 如果连接状态在两步之间消失，把 pending 放回，允许仍在线的客户端继续处理。
            self.pending_tool_pauses
                .lock()
                .expect("pending tool pauses lock poisoned")
                .insert(tool_use_id.to_string());
            ToolPauseResolutionStart::ClientNotConnected
        }
    }

    pub(crate) async fn release_controller(&self, client_id: &str) {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .release(client_id);
        if changed {
            let _ = self.controller_tx.send(controller_id);
        }
    }

    pub(crate) async fn is_controller(&self, client_id: &str) -> bool {
        let presence = self.presence.lock().expect("presence lock poisoned");
        presence.controller_id.as_deref() == Some(client_id)
            && presence.clients.contains_key(client_id)
    }

    pub(crate) async fn controller_id(&self) -> Option<String> {
        self.presence
            .lock()
            .expect("presence lock poisoned")
            .controller_id
            .clone()
    }

    pub(crate) async fn is_client_connected(&self, client_id: &str) -> bool {
        self.presence
            .lock()
            .expect("presence lock poisoned")
            .clients
            .contains_key(client_id)
    }

    pub(crate) async fn client_role(&self, client_id: &str) -> protocol::ClientSessionRole {
        if self.is_controller(client_id).await {
            protocol::ClientSessionRole::Controller
        } else {
            protocol::ClientSessionRole::Observer
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), CoreError> {
        self.core.shutdown().await
    }

    #[cfg(test)]
    pub(crate) fn record_runtime_event_for_test(&self, kind: &str) {
        let event = RuntimeEvent::new(match kind {
            "run_started" => protocol::TypedRuntimeEvent::RunStarted,
            "run_finished" => protocol::TypedRuntimeEvent::RunFinished,
            _ => panic!("unsupported test runtime event kind: {kind}"),
        });
        self.status_projection
            .lock()
            .expect("status projection lock poisoned")
            .record_event(&event, Utc::now());
        let _ = self
            .runtime_event_tx
            .send(SequencedRuntimeEvent { seq: 0, event });
    }
}

fn runtime_event_from_core_with_fallback(event: RuntimeToServerEvent) -> Option<RuntimeEvent> {
    match runtime_event_from_internal(event) {
        Ok(event) => Some(event),
        Err(error) => {
            tracing::error!(error = %error, "failed to encode runtime event");
            let fallback =
                RuntimeToServerEvent::error(format!("Failed to encode runtime event: {error}"));
            match runtime_event_from_internal(fallback) {
                Ok(event) => Some(event),
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "failed to encode fallback runtime event"
                    );
                    None
                }
            }
        }
    }
}

fn broadcast_sequenced_runtime_event(
    event: RuntimeEvent,
    next_seq: &mut u64,
    replay_buffer: &Arc<Mutex<RuntimeReplayBuffer>>,
    status_projection: &Arc<Mutex<RuntimeStatusProjection>>,
    runtime_event_tx: &broadcast::Sender<SequencedRuntimeEvent>,
) {
    let event_kind = event.kind();
    let sequenced = SequencedRuntimeEvent {
        seq: *next_seq,
        event,
    };
    log_runtime_event_broadcast(sequenced.seq, event_kind);
    *next_seq = (*next_seq).saturating_add(1);
    replay_buffer
        .lock()
        .expect("replay buffer lock poisoned")
        .record(sequenced.clone());
    status_projection
        .lock()
        .expect("status projection lock poisoned")
        .record_event(&sequenced.event, Utc::now());
    let _ = runtime_event_tx.send(sequenced);
}

fn log_runtime_event_broadcast(seq: u64, kind: &str) {
    if high_volume_runtime_event(kind) {
        tracing::trace!(seq, event_kind = %kind, "broadcasting runtime event");
    } else {
        tracing::debug!(seq, event_kind = %kind, "broadcasting runtime event");
    }
}

fn high_volume_runtime_event(kind: &str) -> bool {
    matches!(
        kind,
        "thinking_delta" | "text_delta" | "proposed_plan_delta" | "compact_summary_delta"
    )
}

fn effective_session_thinking_effort(
    settings: &Settings,
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> Option<ThinkingEffort> {
    let effort = effort.and_then(|effort| effort.parse().ok());
    settings.effective_thinking_effort_for(provider, model, effort)
}

fn thinking_display_notification(show: bool) -> Notification {
    let message = if show {
        "思考内容展示已开启"
    } else {
        "思考内容展示已关闭"
    };
    Notification::info(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_core::config::settings::Settings;
    use omini_core::config::settings::{ModelEntry, ProviderConfig, UserConfig};
    use omini_domain::config::{ProviderEndpointKind, ThinkingEffort};

    fn test_settings(model: &str) -> Settings {
        let models = HashMap::from([
            (
                "fast".to_string(),
                ModelEntry {
                    name: Some("Fast".to_string()),
                    limit: Some(1000),
                    thinking: Some(false),
                    input_modalities: None,
                },
            ),
            (
                "reasoner".to_string(),
                ModelEntry {
                    name: Some("Reasoner".to_string()),
                    limit: Some(2000),
                    thinking: Some(true),
                    input_modalities: None,
                },
            ),
        ]);
        let config = UserConfig {
            providers: HashMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    name: Some("OpenAI".to_string()),
                    endpoint: ProviderEndpointKind::OpenAI,
                    base_url: "https://openai.example".to_string(),
                    api_key: "test-key".to_string(),
                    models: Some(models),
                },
            )]),
            language: None,
            permissions: None,
            compact: None,
            mcp_servers: HashMap::new(),
        };
        config
            .to_settings(Some("openai"), Some(model), None)
            .expect("settings should build")
    }

    #[test]
    fn snapshot_effort_is_cleared_for_non_thinking_model() {
        let settings = test_settings("fast");

        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "fast", Some("medium")),
            None
        );
    }

    #[test]
    fn snapshot_effort_is_kept_for_thinking_model() {
        let settings = test_settings("reasoner");

        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "reasoner", Some("high")),
            Some(ThinkingEffort::High)
        );
        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "reasoner", Some("none")),
            Some(ThinkingEffort::None)
        );
        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "reasoner", None),
            Some(ThinkingEffort::Medium)
        );
    }
}
