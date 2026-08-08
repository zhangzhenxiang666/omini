use crate::event::bridge::runtime_event_from_runtime_contract_event;
use crate::event::replay::SequencedRuntimeEvent;
use crate::event::tool_pause::apply_tool_pause_update;
use crate::event::{replay::RuntimeReplayBuffer, status::RuntimeStatusProjection};
use crate::thread::{ThreadRuntime, ThreadRuntimeInputs};
use crate::{git, store::Database};
use chrono::Utc;
use omini_config::{Settings, project::ProjectDir};
use omini_core::{AgentCoreThread, CoreError};
use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use tracing::Instrument;

impl ThreadRuntime {
    /// 同步构造：不读 DB、不跨 `.await`，所需的
    /// `ThreadRuntimeInputs` 由调用方提前加载并派生好。
    ///
    /// 这样拆有两个原因:
    /// 1. `create_thread` / `fork_thread_for_plan` 创建的是空 thread，
    ///    在 `build` 里再读一次 DB 是浪费;
    /// 2. 调用方可以在 `threads` 锁外完成异步加载,只在短临界区里
    ///    get / insert runtime cache,保证外层 future 始终 `Send`。
    ///
    /// LLM 输入在 `load_thread_snapshot` 里从当前 `llm_messages` 版本加载，
    /// 这里只原样转发，不再做代码层面的过滤或合并。
    pub fn build(
        project_id: String,
        settings: Settings,
        project: ProjectDir,
        thread_id: String,
        db: Arc<Database>,
        active_profile: domain::events::ActiveProfile,
        inputs: ThreadRuntimeInputs,
    ) -> Result<Self, CoreError> {
        let ThreadRuntimeInputs {
            snapshot: loaded,
            thread_messages,
            llm_context_version,
        } = inputs;
        let thread_usage = loaded.usage;

        let (controller_tx, _) = broadcast::channel(32);
        let (runtime_event_tx, _) = broadcast::channel(512);
        let (server_event_inbox_tx, mut server_event_inbox_rx) = mpsc::unbounded_channel();
        // 核心在 build 时已灌入完整 messages / usage,replay buffer 也要从
        // 这次的 snapshot 推一次 record_snapshot,让后来连接的 ws 不会重复
        // 收到这些已被历史覆盖的事件(目前 snapshot 不发 runtime 事件,
        // 这里保留以防未来 snapshot 再次走 runtime 通道)。
        // `snapshot` 来自 DB(给 user-injection / title 去重用),
        // `thread_messages` 来自当前 LLM context（给 LLM 级去重用）。必须在
        // `core` spawn 之前调用，因为之后会 move。
        let replay_buffer = Arc::new(Mutex::new(RuntimeReplayBuffer::default()));
        {
            let mut buffer = replay_buffer.lock().expect("replay buffer lock poisoned");
            buffer.record_snapshot(&loaded, &thread_messages);
        }
        let core = AgentCoreThread::spawn_for_thread_with_active_profile(
            settings.clone(),
            project.clone(),
            thread_id.clone(),
            active_profile,
            thread_messages,
            llm_context_version,
            thread_usage,
        )?;
        let mut persistence_rx = core
            .take_persistence_receiver()
            .expect("thread persistence receiver already taken");
        let mut tool_pause_rx = core.subscribe();
        let mut runtime_event_rx = core.subscribe();
        let status_projection = Arc::new(Mutex::new(RuntimeStatusProjection::with_active_profile(
            active_profile,
        )));
        let persistence_db = Arc::clone(&db);
        let persisted_replay_buffer = Arc::clone(&replay_buffer);
        let replay_thread_id = thread_id.clone();
        let persistence_project = project.clone();
        let persistence_project_id = project_id;
        // core 发出的持久化事件先落 SQLite，成功后再裁剪 replay，避免重连时漏掉未落盘内容。
        let persistence_handle = tokio::spawn(
            async move {
                while let Some(event) = persistence_rx.recv().await {
                    let result = persistence_db
                        .apply_persistence_event(
                            &event,
                            &persistence_project_id,
                            &persistence_project,
                        )
                        .await;
                    if result.is_ok() {
                        persisted_replay_buffer
                            .lock()
                            .expect("replay buffer lock poisoned")
                            .record_persistence(&replay_thread_id, &event);
                    } else if let Err(error) = &result {
                        tracing::error!(error = %error, "runtime persistence event failed");
                    }
                    if let runtime_contract::RuntimePersistenceEvent::ReplaceLlmContext {
                        expected_version,
                        ack,
                        ..
                    } = event
                    {
                        let _ = ack.send(
                            result
                                .map(|_| expected_version + 1)
                                .map_err(|error| error.to_string()),
                        );
                    }
                }
            }
            .instrument(tracing::debug_span!(
                "thread",
                thread_id = %thread_id,
                task_kind = "persistence_fanout"
            )),
        );
        let runtime_event_fanout_tx = runtime_event_tx.clone();
        let runtime_replay_buffer = Arc::clone(&replay_buffer);
        let runtime_status_projection = Arc::clone(&status_projection);
        let git_branch = Arc::new(Mutex::new(git::detect_git_branch(&settings.cwd)));
        let git_branch_cache = Arc::clone(&git_branch);
        let git_branch_cwd = settings.cwd.clone();
        // runtime 事件加上本地 seq 后再广播，WebSocket 层用 seq 处理 replay/订阅交叠。
        // TurnEnded 时同步检测 git 分支变化，有变化时更新缓存并推送 GitBranchChanged。
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
                                    let is_turn_ended = matches!(event.event, client_proto::TypedRuntimeEvent::TurnEnded);
                                    broadcast_sequenced_runtime_event(
                                        event,
                                        &mut next_seq,
                                        &runtime_replay_buffer,
                                        &runtime_status_projection,
                                        &runtime_event_fanout_tx,
                                    );
                                    if is_turn_ended {
                                        let branch = git::detect_git_branch(&git_branch_cwd);
                                        let mut cache = git_branch_cache.lock()
                                            .expect("git branch cache lock poisoned");
                                        if branch != *cache {
                                            *cache = branch.clone();
                                            drop(cache);
                                            broadcast_sequenced_runtime_event(
                                                client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::GitBranchChanged(
                                                    client_proto::GitBranchChangedEvent { branch },
                                                )),
                                                &mut next_seq,
                                                &runtime_replay_buffer,
                                                &runtime_status_projection,
                                                &runtime_event_fanout_tx,
                                            );
                                        }
                                    }
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
                "thread",
                thread_id = %thread_id,
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
                "thread",
                thread_id = %thread_id,
                task_kind = "tool_pause_watcher"
            )),
        );
        Ok(Self {
            core,
            thread_id,
            project,
            settings,
            db,
            runtime_event_tx,
            server_event_inbox_tx,
            presence: Mutex::new(super::presence::ClientPresence::default()),
            pending_tool_pauses,
            status_projection,
            git_branch,
            replay_buffer,
            controller_tx,
            _persistence_handle: persistence_handle,
            _runtime_event_handle: runtime_event_handle,
            _tool_pause_handle: tool_pause_handle,
        })
    }
}

fn broadcast_sequenced_runtime_event(
    event: client_proto::RuntimeEvent,
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

fn runtime_event_from_core_with_fallback(
    event: runtime_contract::RuntimeToServerEvent,
) -> Option<client_proto::RuntimeEvent> {
    match runtime_event_from_runtime_contract_event(event) {
        Ok(event) => Some(event),
        Err(error) => {
            tracing::error!(error = %error, "failed to encode runtime event");
            let fallback = runtime_contract::RuntimeToServerEvent::error(format!(
                "Failed to encode runtime event: {error}"
            ));
            match runtime_event_from_runtime_contract_event(fallback) {
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
