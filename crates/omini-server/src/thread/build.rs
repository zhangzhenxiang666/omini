use crate::event::bridge::runtime_event_from_runtime_contract_event;
use crate::event::replay::SequencedRuntimeEvent;
use crate::event::tool_pause::apply_tool_pause_update;
use crate::event::{replay::RuntimeReplayBuffer, status::RuntimeStatusProjection};
use crate::thread::{ThreadSession, ThreadSessionInputs};
use crate::{git, store::Store};
use jiff::Timestamp;
use omini_config::{Settings, project::ProjectDir};
use omini_core::CoreError;
use omini_core::execution::AgentHost;
use omini_core::execution::{AgentInstance, AgentInstanceConfig, AgentInstanceLoad, AgentOutput};
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use tracing::Instrument;

impl ThreadSession {
    /// 同步构造：不读 DB、不跨 `.await`，所需的
    /// `ThreadSessionInputs` 由调用方提前加载并派生好。
    ///
    /// 这样拆有两个原因:
    /// 1. `create_thread` / `fork_thread_for_plan` 创建的是空 thread，
    ///    在 `build` 里再读一次 DB 是浪费;
    /// 2. 调用方可以在 `threads` 锁外完成异步加载,只在短临界区里
    ///    get / insert session cache,保证外层 future 始终 `Send`。
    ///
    /// 装配顺序遵循 core 契约：先建实例拿到独占输出接收端，消费者任务就位后
    /// 才启动实例，启动输出不会遗漏。`idle_reclaim` 用于把可回收的空闲会话
    /// 从 manager 缓存中摘除。
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        project_id: String,
        settings: Settings,
        project: ProjectDir,
        thread_id: String,
        db: Arc<Store>,
        active_profile: runtime_contract::thread_domain::ActiveProfile,
        inputs: ThreadSessionInputs,
        idle_reclaim: mpsc::UnboundedSender<(String, omini_core::execution::AgentHandle)>,
    ) -> Result<Arc<Self>, CoreError> {
        let ThreadSessionInputs {
            snapshot: loaded,
            thread_messages,
            llm_context_version,
            background_tasks,
        } = inputs;
        let thread_usage = loaded.usage;
        let agent_tasks = loaded
            .agent_tasks
            .iter()
            .map(|snapshot| snapshot.task.clone())
            .collect();

        let (controller_tx, _) = broadcast::channel(32);
        let (runtime_event_tx, _) = broadcast::channel(512);
        let (server_event_inbox_tx, mut server_event_inbox_rx) = mpsc::unbounded_channel();
        // replay buffer 用装配快照推一次 record_snapshot，让后来连接的 ws 不会
        // 重复收到这些已被历史覆盖的事件。`snapshot` 来自 DB(给
        // user-injection / title 去重用)，`thread_messages` 来自当前 LLM
        // context（给 LLM 级去重用）。
        let replay_buffer = Arc::new(Mutex::new(RuntimeReplayBuffer::default()));
        {
            let mut buffer = replay_buffer.lock().expect("replay buffer lock poisoned");
            buffer.record_snapshot(&loaded, &thread_messages);
        }
        let status_projection = Arc::new(Mutex::new(RuntimeStatusProjection::with_active_profile(
            active_profile,
        )));
        let pending_tool_pauses = Arc::new(Mutex::new(HashSet::new()));
        let presence = Arc::new(Mutex::new(super::presence::ClientPresence::default()));
        let git_branch = Arc::new(Mutex::new(git::detect_git_branch(&settings.cwd)));
        let git_cwd = settings.cwd.clone();
        let session_settings = settings.clone();

        // core 输出的唯一消费者持有宿主；宿主的本地投影（完成通知、排队消息）
        // 经 server_event_inbox 汇入同一条广播流。
        let host = Arc::new(super::SessionHost::new(
            Arc::clone(&db),
            project_id,
            project.clone(),
            thread_id.clone(),
            Arc::clone(&replay_buffer),
            server_event_inbox_tx.clone(),
        ));
        let mut instance = AgentInstance::build(AgentInstanceConfig {
            settings,
            project: project.clone(),
            thread_id: thread_id.clone(),
            active_profile,
            load: AgentInstanceLoad {
                messages: thread_messages,
                llm_context_version,
                usage: thread_usage,
                agent_tasks,
                background_tasks,
            },
            host: host as Arc<dyn AgentHost>,
        })?;
        let handle = instance.handle();
        let mut events = instance.take_events();

        // 消费者任务持有的共享投影句柄。
        let consumer_runtime_event_tx = runtime_event_tx.clone();
        let consumer_replay_buffer = Arc::clone(&replay_buffer);
        let consumer_status_projection = Arc::clone(&status_projection);
        let consumer_pending_tool_pauses = Arc::clone(&pending_tool_pauses);
        let consumer_presence = Arc::clone(&presence);
        let consumer_git_branch = Arc::clone(&git_branch);
        let consumer_thread_id = thread_id.clone();
        let consumer_handle = handle.clone();
        let consumer_span = tracing::debug_span!(
            "thread",
            thread_id = %consumer_thread_id,
            task_kind = "session_consumer"
        );
        let acceptances = Arc::new(super::acceptance::Acceptances::default());
        let consumer_acceptances = Arc::clone(&acceptances);
        let (consumer_done_tx, consumer_finished) = tokio::sync::watch::channel(false);
        let _consumer_handle = tokio::spawn(
            async move {
                // 消费者就位后才启动实例，保证启动输出（装配诊断、首条事件）
                // 从第一个 poll 起就在接收端之后产生。
                instance.start();
                let mut next_seq = 1u64;
                loop {
                    tokio::select! {
                        output = events.recv() => {
                            match output {
                                Some(AgentOutput::Event(event)) => {
                                    let event = *event;
                                    // tool pause 集合在协议转换前用 core 事件维护，
                                    // 与 seq/replay/status 投影同点，无需独立 watcher。
                                    apply_tool_pause_update(&consumer_pending_tool_pauses, &event);
                                    let Some(event) = runtime_event_from_core_with_fallback(event) else {
                                        continue;
                                    };
                                    let is_turn_ended = matches!(event.event, client_proto::TypedRuntimeEvent::TurnEnded);
                                    broadcast_sequenced_runtime_event(
                                        event,
                                        &mut next_seq,
                                        &consumer_replay_buffer,
                                        &consumer_status_projection,
                                        &consumer_runtime_event_tx,
                                    );
                                    if is_turn_ended {
                                        let branch = git::detect_git_branch(&git_cwd);
                                        let mut cache = consumer_git_branch
                                            .lock()
                                            .expect("git branch cache lock poisoned");
                                        if branch != *cache {
                                            *cache = branch.clone();
                                            drop(cache);
                                            broadcast_sequenced_runtime_event(
                                                client_proto::RuntimeEvent::new(
                                                    client_proto::TypedRuntimeEvent::GitBranchChanged(
                                                        client_proto::GitBranchChangedEvent { branch },
                                                    ),
                                                ),
                                                &mut next_seq,
                                                &consumer_replay_buffer,
                                                &consumer_status_projection,
                                                &consumer_runtime_event_tx,
                                            );
                                        }
                                    }
                                }
                                Some(AgentOutput::Idle) => {
                                    // 实例公告空闲：服务过客户端且当前无连接、实例
                                    // 可回收时关闭实例并让 manager 摘除缓存。从未
                                    // 有过连接的会话可能正等待首个客户端连入，跳过。
                                    let (has_clients, served_clients) = {
                                        let presence = consumer_presence
                                            .lock()
                                            .expect("presence lock poisoned");
                                        (
                                            !presence.connection_counts.is_empty(),
                                            presence.has_ever_connected,
                                        )
                                    };
                                    if !has_clients
                                        && served_clients
                                        && consumer_handle.begin_idle_close()
                                    {
                                        tracing::debug!(
                                            thread_id = %consumer_thread_id,
                                            "reclaiming idle session without clients"
                                        );
                                        let _ = consumer_handle.request_close().await;
                                    }
                                }
                                Some(AgentOutput::Closed) | None => break,
                            }
                        }
                        event = server_event_inbox_rx.recv() => {
                            let Some(event) = event else {
                                break;
                            };
                            broadcast_sequenced_runtime_event(
                                event,
                                &mut next_seq,
                                &consumer_replay_buffer,
                                &consumer_status_projection,
                                &consumer_runtime_event_tx,
                            );
                        }
                    }
                }
                // 实例已公告 Closed（或通道结束）：等待实例任务退出，避免悬挂。
                if let Some(join) = instance.join().take() {
                    let _ = join.await;
                }
                consumer_acceptances.wait_finished().await;
                // 宿主本地产生的已提交事实可能晚于 core 尾部入队，关闭前排空。
                while let Ok(event) = server_event_inbox_rx.try_recv() {
                    broadcast_sequenced_runtime_event(event, &mut next_seq,
                        &consumer_replay_buffer, &consumer_status_projection, &consumer_runtime_event_tx);
                }
                consumer_done_tx.send_replace(true);
                let _ = idle_reclaim.send((consumer_thread_id.clone(), consumer_handle));
            }
            .instrument(consumer_span),
        );

        Ok(Arc::new(Self {
            handle,
            acceptances,
            thread_id,
            project,
            settings: session_settings,
            db,
            runtime_event_tx,
            server_event_inbox_tx,
            presence,
            pending_tool_pauses,
            status_projection,
            git_branch,
            replay_buffer,
            controller_tx,
            _consumer_handle,
            consumer_finished,
        }))
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
        .record_event(&sequenced.event, Timestamp::now());
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
