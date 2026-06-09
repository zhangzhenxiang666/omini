use super::*;

#[derive(Clone)]
pub(crate) struct SequencedRuntimeEvent {
    // seq 只在单个 RuntimeSession 内单调递增，用来让 WebSocket replay 和订阅流去重。
    pub(crate) seq: u64,
    pub(crate) event: RuntimeEvent,
}

/// 保存重连时必须补发、但尚未被 snapshot/status 覆盖的运行中尾部和少量 UI 状态。
#[derive(Default)]
pub(super) struct RuntimeReplayBuffer {
    // run_started 之前的用户注入事件先暂存，确保重连客户端能看到刚提交的输入。
    pending_prefix: Vec<SequencedRuntimeEvent>,
    // run_started 是 replay 的锚点；run 结束或 session snapshot 后会清空。
    run_started: Option<SequencedRuntimeEvent>,
    // 当前 turn 尚未被持久化 snapshot 覆盖的尾部增量。
    current_tail: Vec<SequencedRuntimeEvent>,
    // compact 不一定发生在 query run 内；单独保留它的流式尾部供新连接补齐。
    compact_started: Option<SequencedRuntimeEvent>,
    compact_tail: Vec<SequencedRuntimeEvent>,
    // 计划确认发生在 run_finished 后，不能依赖 run tail；保留到任一客户端完成确认。
    pending_plan_approval: Option<SequencedRuntimeEvent>,
    // server/core 的轻量 UI 状态事件不一定落在消息 snapshot 中；每类只保留最新值。
    latest_session_title: Option<SequencedRuntimeEvent>,
    latest_thinking_display: Option<SequencedRuntimeEvent>,
    latest_agent_management: Option<SequencedRuntimeEvent>,
}

impl RuntimeReplayBuffer {
    pub(super) fn record(&mut self, event: SequencedRuntimeEvent) {
        // replay buffer 只保存“重连后需要补发”的运行中尾部事件，落盘内容交给 snapshot。
        match runtime_replay_kind(&event.event) {
            "compact_summary_started" => {
                self.compact_started = Some(event);
                self.compact_tail.clear();
            }
            "compact_summary_delta" => {
                if self.compact_started.is_some() {
                    self.compact_tail.push(event);
                }
            }
            "compact_summary_finished" => {
                if self.compact_started.is_some() {
                    self.compact_tail.push(event);
                }
            }
            "compact_summary_failed" => {
                self.clear_compact_tail();
            }
            "plan_submitted" => {
                self.pending_plan_approval = Some(event);
            }
            "plan_approval_resolved" => {
                if self.pending_plan_matches(&event) {
                    self.pending_plan_approval = None;
                }
            }
            "session_title_changed" => {
                self.latest_session_title = Some(event);
            }
            "thinking_display_changed" => {
                self.latest_thinking_display = Some(event);
            }
            "agent_management_updated" => {
                self.latest_agent_management = Some(event);
            }
            kind if is_session_snapshot_kind(kind) => {
                if self.run_started.is_some() || self.compact_started.is_some() {
                    self.clear();
                } else {
                    self.pending_plan_approval = None;
                }
            }
            "run_finished" => self.clear(),
            "user_message_injected" => {
                if self.run_started.is_some() {
                    self.current_tail.push(event);
                } else {
                    self.pending_prefix.push(event);
                }
            }
            "run_started" => {
                self.run_started = Some(event);
                self.current_tail.clear();
            }
            "turn_started" => {
                if self.run_started.is_some() {
                    self.current_tail.clear();
                    self.current_tail.push(event);
                }
            }
            "turn_ended" => {
                if self.run_started.is_some() {
                    self.pending_prefix.clear();
                    self.current_tail.clear();
                    self.current_tail.push(event);
                }
            }
            _ => {
                if self.run_started.is_some() {
                    self.current_tail.push(event);
                }
            }
        }
    }

    pub(super) fn replay(&self) -> Vec<SequencedRuntimeEvent> {
        let mut replay = Vec::with_capacity(
            self.pending_prefix.len()
                + usize::from(self.run_started.is_some())
                + self.current_tail.len()
                + usize::from(self.compact_started.is_some())
                + self.compact_tail.len()
                + usize::from(self.pending_plan_approval.is_some())
                + usize::from(self.latest_session_title.is_some())
                + usize::from(self.latest_thinking_display.is_some())
                + usize::from(self.latest_agent_management.is_some()),
        );
        replay.extend(self.pending_prefix.iter().cloned());
        if let Some(run_started) = &self.run_started {
            replay.push(run_started.clone());
        }
        replay.extend(self.current_tail.iter().cloned());
        if let Some(plan) = &self.pending_plan_approval {
            replay.push(plan.clone());
        }
        if let Some(compact_started) = &self.compact_started {
            replay.push(compact_started.clone());
        }
        replay.extend(self.compact_tail.iter().cloned());
        if let Some(event) = &self.latest_session_title {
            replay.push(event.clone());
        }
        if let Some(event) = &self.latest_thinking_display {
            replay.push(event.clone());
        }
        if let Some(event) = &self.latest_agent_management {
            replay.push(event.clone());
        }
        replay.sort_by_key(|event| event.seq);
        replay
    }

    pub(super) fn record_persistence(
        &mut self,
        owner_session_id: &str,
        event: &RuntimePersistenceEvent,
    ) {
        // 持久化成功意味着对应 UI 片段下一次会从 snapshot 恢复，应从 replay 中裁掉。
        match event {
            RuntimePersistenceEvent::InsertMessage {
                session_id,
                role,
                blocks,
                ..
            } if session_id == owner_session_id => {
                if role == "assistant" {
                    self.drop_current_assistant_tail();
                } else if blocks.iter().any(ContentBlock::is_tool_result) {
                    self.drop_persisted_tool_results();
                } else {
                    self.drop_pending_user_injection();
                }
            }
            RuntimePersistenceEvent::InsertDisplayMessage { session_id, .. }
                if session_id == owner_session_id =>
            {
                self.drop_pending_user_injection();
            }
            RuntimePersistenceEvent::InsertCompactSummaryMessage { session_id, .. }
                if session_id == owner_session_id =>
            {
                self.drop_current_compact_summary_tail();
            }
            _ => {}
        }
    }

    pub(super) fn record_snapshot(&mut self, snapshot: &LoadedSession) {
        // 新连接发 snapshot 前再做一次裁剪，覆盖持久化事件和 snapshot 生成之间的竞态。
        self.drop_user_injections_in_snapshot(snapshot);
        self.drop_session_title_in_snapshot(snapshot);
        if self.current_assistant_tail_is_in_snapshot(snapshot) {
            self.drop_current_assistant_tail();
        }
        if self.current_tool_results_are_in_snapshot(snapshot) {
            self.drop_persisted_tool_results();
        }
    }

    fn clear(&mut self) {
        self.pending_prefix.clear();
        self.run_started = None;
        self.current_tail.clear();
        self.clear_compact_tail();
        self.pending_plan_approval = None;
    }

    fn clear_compact_tail(&mut self) {
        self.compact_started = None;
        self.compact_tail.clear();
    }

    fn pending_plan_matches(&self, event: &SequencedRuntimeEvent) -> bool {
        let Some(pending) = &self.pending_plan_approval else {
            return false;
        };
        let Some(pending_plan_id) = plan_submitted_payload(&pending.event).map(|plan| plan.plan_id)
        else {
            return true;
        };
        plan_approval_resolved_plan_id(&event.event)
            .map(|resolved_plan_id| resolved_plan_id == pending_plan_id)
            .unwrap_or(true)
    }

    fn drop_pending_user_injection(&mut self) {
        self.pending_prefix
            .retain(|event| runtime_replay_kind(&event.event) != "user_message_injected");
        self.current_tail
            .retain(|event| runtime_replay_kind(&event.event) != "user_message_injected");
    }

    fn drop_current_assistant_tail(&mut self) {
        self.current_tail.retain(|event| {
            !matches!(
                runtime_replay_kind(&event.event),
                "thinking_delta" | "text_delta" | "proposed_plan_delta" | "tool_use"
            )
        });
    }

    fn drop_persisted_tool_results(&mut self) {
        self.current_tail
            .retain(|event| runtime_replay_kind(&event.event) != "tool_result");
    }

    fn drop_current_compact_summary_tail(&mut self) {
        self.current_tail.retain(|event| {
            !matches!(
                runtime_replay_kind(&event.event),
                "compact_summary_started" | "compact_summary_delta" | "compact_summary_finished"
            )
        });
        self.clear_compact_tail();
    }

    fn drop_user_injections_in_snapshot(&mut self, snapshot: &LoadedSession) {
        self.pending_prefix
            .retain(|event| !user_injection_is_in_snapshot(event, snapshot));
        self.current_tail
            .retain(|event| !user_injection_is_in_snapshot(event, snapshot));
    }

    fn drop_session_title_in_snapshot(&mut self, snapshot: &LoadedSession) {
        let Some(event) = &self.latest_session_title else {
            return;
        };
        if session_title_payload(&event.event) == Some(snapshot.title.as_ref()) {
            self.latest_session_title = None;
        }
    }

    fn current_assistant_tail_is_in_snapshot(&self, snapshot: &LoadedSession) -> bool {
        let blocks = assistant_tail_blocks(&self.current_tail);
        !blocks.is_empty()
            && snapshot.messages.iter().any(|item| {
                matches!(
                    item,
                    HistoryItem::Message(Message {
                        role: Role::Assistant,
                        content,
                    }) if *content == blocks
                )
            })
    }

    fn current_tool_results_are_in_snapshot(&self, snapshot: &LoadedSession) -> bool {
        let blocks = tool_result_tail_blocks(&self.current_tail);
        !blocks.is_empty()
            && snapshot.messages.iter().any(|item| {
                matches!(
                    item,
                    HistoryItem::Message(Message {
                        role: Role::User,
                        content,
                    }) if *content == blocks
                )
            })
    }
}

pub(super) fn runtime_replay_kind(event: &RuntimeEvent) -> &'static str {
    event.kind()
}

pub(super) fn is_session_snapshot_kind(kind: &str) -> bool {
    kind == "session_snapshot"
}

/// 判断待 replay 的用户注入事件是否已经出现在持久化 snapshot 中。
fn user_injection_is_in_snapshot(event: &SequencedRuntimeEvent, snapshot: &LoadedSession) -> bool {
    let protocol::TypedRuntimeEvent::UserMessageInjected { item, .. } = &event.event.event else {
        return false;
    };
    snapshot.messages.iter().any(|message| message == item)
}

fn session_title_payload(event: &RuntimeEvent) -> Option<Option<&String>> {
    let protocol::TypedRuntimeEvent::SessionTitleChanged(event) = &event.event else {
        return None;
    };
    Some(event.title.as_ref())
}

/// 把当前 assistant 流式尾部重组为完整内容块，供 snapshot 去重比较。
fn assistant_tail_blocks(events: &[SequencedRuntimeEvent]) -> Vec<ContentBlock> {
    // 增量事件需要还原成完整 ContentBlock，才能和 snapshot 中的 assistant message 比较。
    let mut blocks = Vec::new();
    for event in events {
        match &event.event.event {
            protocol::TypedRuntimeEvent::ThinkingDelta(event) => {
                push_delta_block(&mut blocks, &event.delta, true)
            }
            protocol::TypedRuntimeEvent::TextDelta(event) => {
                push_delta_block(&mut blocks, &event.delta, false)
            }
            protocol::TypedRuntimeEvent::ToolUse(tool_use) => {
                blocks.push(ContentBlock::ToolUse(tool_use.clone()));
            }
            _ => {}
        }
    }
    blocks
}

/// 收集当前尾部中尚未被 snapshot 覆盖的工具结果块。
fn tool_result_tail_blocks(events: &[SequencedRuntimeEvent]) -> Vec<ContentBlock> {
    events
        .iter()
        .filter_map(|event| match &event.event.event {
            protocol::TypedRuntimeEvent::ToolResult(tool_result) => {
                Some(ContentBlock::ToolResult(tool_result.clone()))
            }
            _ => None,
        })
        .collect()
}

/// 将连续文本或 thinking delta 合并成可比较的 `ContentBlock`。
fn push_delta_block(blocks: &mut Vec<ContentBlock>, delta: &str, thinking: bool) {
    match (thinking, blocks.last_mut()) {
        (true, Some(ContentBlock::Thinking(block))) => block.thinking.push_str(delta),
        (false, Some(ContentBlock::Text(block))) => block.text.push_str(delta),
        (true, _) => blocks.push(ContentBlock::from_thinking(delta.to_string())),
        (false, _) => blocks.push(ContentBlock::from_text(delta.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_core::RuntimeToServerEvent;
    use omini_domain::events as event_types;
    use omini_domain::events::{
        CompactEvent, CompactSummaryDeltaEvent, CompactTrigger, SessionUsageSnapshot, SubmittedPlan,
    };
    use omini_domain::message::{ToolResultBlock, ToolUseBlock};

    fn sequenced(seq: u64, kind: &str) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: typed_test_event(kind),
        }
    }

    fn delta(seq: u64, kind: &str, text: &str) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: RuntimeEvent::new(match kind {
                "thinking_delta" => {
                    protocol::TypedRuntimeEvent::ThinkingDelta(protocol::RuntimeDeltaEvent {
                        delta: text.to_string(),
                    })
                }
                "text_delta" => {
                    protocol::TypedRuntimeEvent::TextDelta(protocol::RuntimeDeltaEvent {
                        delta: text.to_string(),
                    })
                }
                "proposed_plan_delta" => {
                    protocol::TypedRuntimeEvent::ProposedPlanDelta(protocol::RuntimeDeltaEvent {
                        delta: text.to_string(),
                    })
                }
                _ => panic!("unsupported delta test event kind: {kind}"),
            }),
        }
    }

    fn typed_test_event(kind: &str) -> RuntimeEvent {
        RuntimeEvent::new(match kind {
            "notification" => {
                protocol::TypedRuntimeEvent::Notification(protocol::NotificationEvent {
                    level: protocol::NotificationLevel::Info,
                    message: "notice".to_string(),
                    details: Vec::new(),
                })
            }
            "user_message_injected" => protocol::TypedRuntimeEvent::UserMessageInjected {
                item: HistoryItem::Message(Message::from_user_text("hello".to_string())),
                client_echo_id: None,
            },
            "run_started" => protocol::TypedRuntimeEvent::RunStarted,
            "run_finished" => protocol::TypedRuntimeEvent::RunFinished,
            "turn_started" => protocol::TypedRuntimeEvent::TurnStarted,
            "turn_ended" => protocol::TypedRuntimeEvent::TurnEnded,
            "tool_use" => protocol::TypedRuntimeEvent::ToolUse(ToolUseBlock {
                id: "tool_1".to_string(),
                name: "read".to_string(),
                input: HashMap::new(),
            }),
            "tool_result" => protocol::TypedRuntimeEvent::ToolResult(ToolResultBlock {
                tool_use_id: "tool_1".to_string(),
                is_error: false,
                content: "done".to_string(),
                metadata: None,
            }),
            "tool_pause_requested" => {
                protocol::TypedRuntimeEvent::ToolPauseRequested(event_types::ToolPauseRequest {
                    tool_use_id: "tool_1".to_string(),
                    preview_tool_use_id: None,
                    tool_name: "bash".to_string(),
                    permission_source: None,
                    source_session_id: None,
                    source_agent_label: None,
                    kind: event_types::ToolPauseKind::Permission(
                        event_types::PermissionPreview::Custom {
                            tool_name: "bash".to_string(),
                            payload: serde_json::Map::new(),
                        },
                    ),
                })
            }
            "session_snapshot" => {
                protocol::TypedRuntimeEvent::SessionSnapshot(protocol::SessionSnapshotEvent {
                    session_id: Some("s1".to_string()),
                    messages: Vec::new(),
                    subagents: Vec::new(),
                    usage: SessionUsageSnapshot::default(),
                })
            }
            _ => panic!("unsupported test event kind: {kind}"),
        })
    }

    fn runtime_event(seq: u64, event: RuntimeToServerEvent) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: runtime_event_from_internal(event).expect("event should encode"),
        }
    }

    fn replay_kinds(buffer: &RuntimeReplayBuffer) -> Vec<String> {
        buffer
            .replay()
            .into_iter()
            .map(|event| event.event.kind().to_string())
            .collect()
    }

    fn snapshot(messages: Vec<HistoryItem>) -> LoadedSession {
        snapshot_with_title(None, messages)
    }

    fn snapshot_with_title(title: Option<String>, messages: Vec<HistoryItem>) -> LoadedSession {
        LoadedSession {
            session_id: "s1".to_string(),
            provider: "main".to_string(),
            model: "test-model".to_string(),
            thinking_effort: None,
            active_profile: ActiveProfile::Main,
            title,
            messages,
            subagents: Vec::new(),
            usage: SessionUsageSnapshot::default(),
        }
    }

    fn persisted_message(
        session_id: &str,
        role: &str,
        blocks: Vec<ContentBlock>,
    ) -> RuntimePersistenceEvent {
        RuntimePersistenceEvent::InsertMessage {
            session_id: session_id.to_string(),
            role: role.to_string(),
            blocks,
            kind: "normal".to_string(),
            created_at: chrono::Utc::now(),
            blocks_dir: PathBuf::new(),
        }
    }

    #[test]
    fn replay_buffer_ignores_idle_runtime_events() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "notification"));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_replays_latest_server_local_state_events() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(runtime_event(
            1,
            RuntimeToServerEvent::SessionTitleChanged {
                title: Some("old".to_string()),
            },
        ));
        buffer.record(SequencedRuntimeEvent {
            seq: 2,
            event: thinking_display_changed_event(false),
        });
        buffer.record(runtime_event(
            3,
            RuntimeToServerEvent::AgentManagementUpdated {
                records: Vec::new(),
            },
        ));
        buffer.record(runtime_event(
            4,
            RuntimeToServerEvent::SessionTitleChanged {
                title: Some("new".to_string()),
            },
        ));

        let replay = buffer.replay();

        assert_eq!(
            replay.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(
            replay
                .iter()
                .map(|event| event.event.kind())
                .collect::<Vec<_>>(),
            vec![
                "thinking_display_changed",
                "agent_management_updated",
                "session_title_changed"
            ]
        );
        assert!(matches!(
            &replay[2].event.event,
            protocol::TypedRuntimeEvent::SessionTitleChanged(event)
                if event.title.as_deref() == Some("new")
        ));
    }

    #[test]
    fn replay_buffer_drops_session_title_recovered_by_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(runtime_event(
            1,
            RuntimeToServerEvent::SessionTitleChanged {
                title: Some("hello".to_string()),
            },
        ));
        buffer.record_snapshot(&snapshot_with_title(Some("hello".to_string()), Vec::new()));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_replays_pending_run_tail() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "user_message_injected"));
        buffer.record(sequenced(2, "run_started"));
        buffer.record(sequenced(3, "turn_started"));
        buffer.record(delta(4, "text_delta", "hello"));

        assert_eq!(
            replay_kinds(&buffer),
            vec![
                "user_message_injected",
                "run_started",
                "turn_started",
                "text_delta"
            ]
        );
    }

    #[test]
    fn replay_buffer_replays_pending_plan_until_resolved() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(runtime_event(
            1,
            RuntimeToServerEvent::PlanSubmitted(SubmittedPlan {
                id: "plan_1".to_string(),
                title: "Plan".to_string(),
                markdown: "# Plan".to_string(),
                path: PathBuf::new(),
                created_at: Utc::now(),
            }),
        ));

        assert_eq!(replay_kinds(&buffer), vec!["plan_submitted"]);

        buffer.record(runtime_event(
            2,
            RuntimeToServerEvent::PlanApprovalResolved {
                plan_id: "plan_1".to_string(),
                action: protocol::PlanApprovalAction::ContinueDiscussing,
            },
        ));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_preserves_pending_user_until_run_starts() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "user_message_injected"));
        buffer.record(sequenced(2, "session_snapshot"));
        buffer.record(sequenced(3, "run_started"));

        assert_eq!(
            replay_kinds(&buffer),
            vec!["user_message_injected", "run_started"]
        );
    }

    #[test]
    fn replay_buffer_drops_user_injection_found_in_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();
        let item = HistoryItem::Message(Message::from_user_text("hello".to_string()));
        let event = runtime_event_from_internal(RuntimeToServerEvent::UserMessageInjected {
            item: item.clone(),
            client_echo_id: Some("echo-1".to_string()),
        })
        .expect("event should encode");

        buffer.record(SequencedRuntimeEvent { seq: 1, event });
        buffer.record_snapshot(&snapshot(vec![item]));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_drops_user_injection_after_persistence() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "user_message_injected"));
        buffer.record_persistence(
            "s1",
            &persisted_message(
                "s1",
                "user",
                vec![ContentBlock::from_text("hello".to_string())],
            ),
        );

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_drops_persisted_assistant_tail() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "thinking_delta", "thinking"));
        buffer.record(delta(4, "text_delta", "answer"));
        buffer.record(sequenced(5, "tool_use"));
        buffer.record(sequenced(6, "tool_pause_requested"));

        buffer.record_persistence(
            "s1",
            &persisted_message(
                "s1",
                "assistant",
                vec![
                    ContentBlock::from_thinking("thinking".to_string()),
                    ContentBlock::from_text("answer".to_string()),
                    ContentBlock::from_tool_use(
                        "tool_1".to_string(),
                        "read".to_string(),
                        HashMap::new(),
                    ),
                ],
            ),
        );

        assert_eq!(
            replay_kinds(&buffer),
            vec!["run_started", "turn_started", "tool_pause_requested"]
        );
    }

    #[test]
    fn replay_buffer_drops_assistant_tail_found_in_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();
        let assistant = Message::new(
            Role::Assistant,
            vec![
                ContentBlock::from_thinking("thinking".to_string()),
                ContentBlock::from_text("answer".to_string()),
                ContentBlock::from_tool_use(
                    "tool_1".to_string(),
                    "read".to_string(),
                    HashMap::new(),
                ),
            ],
        );

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "thinking_delta", "thinking"));
        buffer.record(delta(4, "text_delta", "answer"));
        buffer.record(SequencedRuntimeEvent {
            seq: 5,
            event: runtime_event_from_internal(RuntimeToServerEvent::ToolUse(
                match assistant.content[2].clone() {
                    ContentBlock::ToolUse(tool_use) => tool_use,
                    _ => unreachable!(),
                },
            ))
            .expect("event should encode"),
        });

        buffer.record_snapshot(&snapshot(vec![HistoryItem::Message(assistant)]));

        assert_eq!(replay_kinds(&buffer), vec!["run_started", "turn_started"]);
    }

    #[test]
    fn replay_buffer_drops_tool_result_after_persistence() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(sequenced(3, "tool_result"));

        buffer.record_persistence(
            "s1",
            &persisted_message(
                "s1",
                "user",
                vec![ContentBlock::from_tool_result(
                    "tool_1".to_string(),
                    false,
                    "done".to_string(),
                )],
            ),
        );

        assert_eq!(replay_kinds(&buffer), vec!["run_started", "turn_started"]);
    }

    #[test]
    fn replay_buffer_drops_tool_result_found_in_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();
        let tool_result =
            ContentBlock::from_tool_result("tool_1".to_string(), false, "done".to_string());
        let ContentBlock::ToolResult(tool_result_event) = tool_result.clone() else {
            unreachable!();
        };

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(SequencedRuntimeEvent {
            seq: 3,
            event: runtime_event_from_internal(RuntimeToServerEvent::ToolResult(tool_result_event))
                .expect("event should encode"),
        });
        buffer.record_snapshot(&snapshot(vec![HistoryItem::Message(Message::new(
            Role::User,
            vec![tool_result],
        ))]));

        assert_eq!(replay_kinds(&buffer), vec!["run_started", "turn_started"]);
    }

    #[test]
    fn replay_buffer_drops_completed_turn_delta() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "text_delta", "done"));
        buffer.record(sequenced(4, "turn_ended"));

        assert_eq!(replay_kinds(&buffer), vec!["run_started", "turn_ended"]);
    }

    #[test]
    fn replay_buffer_keeps_only_current_turn_tail() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "text_delta", "first"));
        buffer.record(sequenced(4, "turn_ended"));
        buffer.record(sequenced(5, "turn_started"));
        buffer.record(delta(6, "text_delta", "second"));

        let replay = buffer.replay();
        assert_eq!(
            replay
                .iter()
                .map(|event| event.event.kind())
                .collect::<Vec<_>>(),
            vec!["run_started", "turn_started", "text_delta"]
        );
        assert!(matches!(
            &replay[2].event.event,
            protocol::TypedRuntimeEvent::TextDelta(event) if event.delta == "second"
        ));
    }

    #[test]
    fn replay_buffer_replays_in_progress_compact_tail_without_run() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(runtime_event(
            1,
            RuntimeToServerEvent::CompactSummaryStarted(CompactEvent {
                trigger: CompactTrigger::Manual,
                session_id: Some("s1".to_string()),
                agent_label: None,
            }),
        ));
        buffer.record(runtime_event(
            2,
            RuntimeToServerEvent::CompactSummaryDelta(CompactSummaryDeltaEvent {
                trigger: CompactTrigger::Manual,
                delta: "partial".to_string(),
                session_id: Some("s1".to_string()),
                agent_label: None,
            }),
        ));

        let replay = buffer.replay();
        assert_eq!(
            replay
                .iter()
                .map(|event| event.event.kind())
                .collect::<Vec<_>>(),
            vec!["compact_summary_started", "compact_summary_delta"]
        );
        assert!(matches!(
            &replay[1].event.event,
            protocol::TypedRuntimeEvent::CompactSummaryDelta(event) if event.delta == "partial"
        ));
    }

    #[test]
    fn replay_buffer_clears_after_run_finished() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "text_delta", "hello"));
        buffer.record(sequenced(4, "run_finished"));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_clears_active_run_on_session_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "text_delta", "hello"));
        buffer.record(sequenced(4, "session_snapshot"));

        assert!(buffer.replay().is_empty());
    }
}
