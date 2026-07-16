use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;

#[derive(Clone)]
pub struct SequencedRuntimeEvent {
    // seq 只在单个 RuntimeSession 内单调递增，用来让 WebSocket replay 和订阅流去重。
    pub seq: u64,
    pub event: client_proto::RuntimeEvent,
}

/// 保存重连时必须补发、但尚未被 snapshot/status 覆盖的运行中尾部和少量 UI 状态。
#[derive(Default)]
pub struct RuntimeReplayBuffer {
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
    pub fn record(&mut self, event: SequencedRuntimeEvent) {
        // replay buffer 只保存“重连后需要补发”的运行中尾部事件，落盘内容交给 snapshot。
        match event.event.kind() {
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
            "session_snapshot" => {
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

    pub fn replay(&self) -> Vec<SequencedRuntimeEvent> {
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

    pub fn record_persistence(
        &mut self,
        owner_session_id: &str,
        event: &runtime_contract::RuntimePersistenceEvent,
    ) {
        // 持久化成功意味着对应 UI 片段下一次会从 snapshot 恢复，应从 replay 中裁掉。
        match event {
            runtime_contract::RuntimePersistenceEvent::InsertMessage {
                session_id,
                role,
                blocks,
                ..
            } if session_id == owner_session_id => {
                if role == "assistant" {
                    self.drop_current_assistant_tail();
                } else if blocks
                    .iter()
                    .any(domain::message::ContentBlock::is_tool_result)
                {
                    self.drop_persisted_tool_results();
                } else {
                    self.drop_pending_user_injection();
                }
            }
            runtime_contract::RuntimePersistenceEvent::InsertDisplayMessage {
                session_id, ..
            } if session_id == owner_session_id => {
                self.drop_pending_user_injection();
            }
            runtime_contract::RuntimePersistenceEvent::InsertCompactSummaryMessage {
                session_id,
                ..
            } if session_id == owner_session_id => {
                self.drop_current_compact_summary_tail();
            }
            _ => {}
        }
    }

    pub fn record_snapshot(
        &mut self,
        snapshot: &domain::events::LoadedSession,
        session_messages: &[domain::message::Message],
    ) {
        // 新连接发 snapshot 前再做一次裁剪，覆盖持久化事件和 snapshot 生成之间的竞态。
        self.drop_user_injections_in_snapshot(snapshot);
        self.drop_session_title_in_snapshot(snapshot);
        // LLM 级去重走 jsonl 路径(`session_messages` 来自
        // `SessionDir::load_history()`),不再用 DB 加载的 HistoryItem 集合。
        if self.current_assistant_tail_is_in_snapshot(session_messages) {
            self.drop_current_assistant_tail();
        }
        if self.current_tool_results_are_in_snapshot(session_messages) {
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
        let Some(pending_plan_id) =
            super::status::plan_submitted_payload(&pending.event).map(|plan| plan.plan_id)
        else {
            return true;
        };
        super::status::plan_approval_resolved_plan_id(&event.event)
            .map(|resolved_plan_id| resolved_plan_id == pending_plan_id)
            .unwrap_or(true)
    }

    fn drop_pending_user_injection(&mut self) {
        self.pending_prefix
            .retain(|event| event.event.kind() != "user_message_injected");
        self.current_tail
            .retain(|event| event.event.kind() != "user_message_injected");
    }

    fn drop_current_assistant_tail(&mut self) {
        self.current_tail.retain(|event| {
            !matches!(
                event.event.kind(),
                "thinking_delta" | "text_delta" | "proposed_plan_delta" | "tool_use"
            )
        });
    }

    fn drop_persisted_tool_results(&mut self) {
        self.current_tail
            .retain(|event| event.event.kind() != "tool_result");
    }

    fn drop_current_compact_summary_tail(&mut self) {
        self.current_tail.retain(|event| {
            !matches!(
                event.event.kind(),
                "compact_summary_started" | "compact_summary_delta" | "compact_summary_finished"
            )
        });
        self.clear_compact_tail();
    }

    fn drop_user_injections_in_snapshot(&mut self, snapshot: &domain::events::LoadedSession) {
        self.pending_prefix
            .retain(|event| !user_injection_is_in_snapshot(event, snapshot));
        self.current_tail
            .retain(|event| !user_injection_is_in_snapshot(event, snapshot));
    }

    fn drop_session_title_in_snapshot(&mut self, snapshot: &domain::events::LoadedSession) {
        let Some(event) = &self.latest_session_title else {
            return;
        };
        if session_title_payload(&event.event) == Some(snapshot.title.as_ref()) {
            self.latest_session_title = None;
        }
    }

    fn current_assistant_tail_is_in_snapshot(
        &self,
        session_messages: &[domain::message::Message],
    ) -> bool {
        let blocks = assistant_tail_blocks(&self.current_tail);
        !blocks.is_empty()
            && session_messages.iter().any(|message| {
                message.role == domain::message::Role::Assistant && message.content == blocks
            })
    }

    fn current_tool_results_are_in_snapshot(
        &self,
        session_messages: &[domain::message::Message],
    ) -> bool {
        let blocks = tool_result_tail_blocks(&self.current_tail);
        !blocks.is_empty()
            && session_messages.iter().any(|message| {
                message.role == domain::message::Role::User && message.content == blocks
            })
    }
}

/// 判断待 replay 的用户注入事件是否已经出现在持久化 snapshot 中。
fn user_injection_is_in_snapshot(
    event: &SequencedRuntimeEvent,
    snapshot: &domain::events::LoadedSession,
) -> bool {
    let client_proto::TypedRuntimeEvent::UserMessageInjected { item, .. } = &event.event.event
    else {
        return false;
    };
    snapshot.messages.iter().any(|message| message == item)
}

fn session_title_payload(event: &client_proto::RuntimeEvent) -> Option<Option<&String>> {
    let client_proto::TypedRuntimeEvent::SessionTitleChanged(event) = &event.event else {
        return None;
    };
    Some(event.title.as_ref())
}

/// 把当前 assistant 流式尾部重组为完整内容块，供 snapshot 去重比较。
fn assistant_tail_blocks(events: &[SequencedRuntimeEvent]) -> Vec<domain::message::ContentBlock> {
    // 增量事件需要还原成完整 ContentBlock，才能和 snapshot 中的 assistant message 比较。
    let mut blocks = Vec::new();
    for event in events {
        match &event.event.event {
            client_proto::TypedRuntimeEvent::ThinkingDelta(event) => {
                push_delta_block(&mut blocks, &event.delta, true)
            }
            client_proto::TypedRuntimeEvent::TextDelta(event) => {
                push_delta_block(&mut blocks, &event.delta, false)
            }
            client_proto::TypedRuntimeEvent::ToolUse(tool_use) => {
                blocks.push(domain::message::ContentBlock::ToolUse(tool_use.clone()));
            }
            _ => {}
        }
    }
    blocks
}

/// 收集当前尾部中尚未被 snapshot 覆盖的工具结果块。
fn tool_result_tail_blocks(events: &[SequencedRuntimeEvent]) -> Vec<domain::message::ContentBlock> {
    events
        .iter()
        .filter_map(|event| match &event.event.event {
            client_proto::TypedRuntimeEvent::ToolResult(tool_result) => Some(
                domain::message::ContentBlock::ToolResult(tool_result.clone()),
            ),
            _ => None,
        })
        .collect()
}

/// 将连续文本或 thinking delta 合并成可比较的 `ContentBlock`。
fn push_delta_block(blocks: &mut Vec<domain::message::ContentBlock>, delta: &str, thinking: bool) {
    match (thinking, blocks.last_mut()) {
        (true, Some(domain::message::ContentBlock::Thinking(block))) => {
            block.thinking.push_str(delta)
        }
        (false, Some(domain::message::ContentBlock::Text(block))) => block.text.push_str(delta),
        (true, _) => blocks.push(domain::message::ContentBlock::from_thinking(
            delta.to_string(),
        )),
        (false, _) => blocks.push(domain::message::ContentBlock::from_text(delta.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::event::bridge::{
        runtime_event_from_runtime_contract_event, session_title_changed_protocol_event,
        thinking_display_changed_protocol_event,
    };
    use std::{collections::HashMap, path::PathBuf};

    fn sequenced(seq: u64, kind: &str) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: typed_test_event(kind),
        }
    }

    fn delta(seq: u64, kind: &str, text: &str) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: client_proto::RuntimeEvent::new(match kind {
                "thinking_delta" => client_proto::TypedRuntimeEvent::ThinkingDelta(
                    client_proto::RuntimeDeltaEvent {
                        delta: text.to_string(),
                    },
                ),
                "text_delta" => {
                    client_proto::TypedRuntimeEvent::TextDelta(client_proto::RuntimeDeltaEvent {
                        delta: text.to_string(),
                    })
                }
                "proposed_plan_delta" => client_proto::TypedRuntimeEvent::ProposedPlanDelta(
                    client_proto::RuntimeDeltaEvent {
                        delta: text.to_string(),
                    },
                ),
                _ => panic!("unsupported delta test event kind: {kind}"),
            }),
        }
    }

    fn typed_test_event(kind: &str) -> client_proto::RuntimeEvent {
        client_proto::RuntimeEvent::new(match kind {
            "notification" => {
                client_proto::TypedRuntimeEvent::Notification(client_proto::NotificationEvent {
                    level: client_proto::NotificationLevel::Info,
                    message: "notice".to_string(),
                    details: Vec::new(),
                })
            }
            "user_message_injected" => client_proto::TypedRuntimeEvent::UserMessageInjected {
                item: domain::display::HistoryItem::Message(
                    domain::message::Message::from_user_text("hello".to_string()),
                ),
                client_echo_id: None,
            },
            "run_started" => client_proto::TypedRuntimeEvent::RunStarted,
            "run_finished" => client_proto::TypedRuntimeEvent::RunFinished,
            "turn_started" => client_proto::TypedRuntimeEvent::TurnStarted,
            "turn_ended" => client_proto::TypedRuntimeEvent::TurnEnded,
            "tool_use" => client_proto::TypedRuntimeEvent::ToolUse(domain::message::ToolUseBlock {
                id: "tool_1".to_string(),
                name: "read".to_string(),
                input: HashMap::new(),
            }),
            "tool_result" => {
                client_proto::TypedRuntimeEvent::ToolResult(domain::message::ToolResultBlock {
                    tool_use_id: "tool_1".to_string(),
                    is_error: false,
                    content: "done".to_string(),
                    metadata: None,
                })
            }
            "tool_pause_requested" => client_proto::TypedRuntimeEvent::ToolPauseRequested(
                domain::events::ToolPauseRequest {
                    tool_use_id: "tool_1".to_string(),
                    preview_tool_use_id: None,
                    tool_name: "bash".to_string(),
                    permission_source: None,
                    source_session_id: None,
                    source_agent_label: None,
                    kind: domain::events::ToolPauseKind::Permission(
                        domain::events::PermissionPreview::Custom {
                            tool_name: "bash".to_string(),
                            payload: serde_json::Map::new(),
                        },
                    ),
                },
            ),
            "session_snapshot" => client_proto::TypedRuntimeEvent::SessionSnapshot(
                client_proto::SessionSnapshotEvent {
                    session_id: Some("s1".to_string()),
                    messages: Vec::new(),
                    subagents: Vec::new(),
                    usage: domain::events::SessionUsageSnapshot::default(),
                },
            ),
            _ => panic!("unsupported test event kind: {kind}"),
        })
    }

    fn runtime_event(
        seq: u64,
        event: runtime_contract::RuntimeToServerEvent,
    ) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: runtime_event_from_runtime_contract_event(event).expect("event should encode"),
        }
    }

    /// 构造一个已经编码好的 `RuntimeEvent`(协议层),不走 `RuntimeToServerEvent`。
    /// 适用于测试 server-side 直发的事件(title 变化、git 分支等)。
    fn sequenced_runtime_event(
        seq: u64,
        event: client_proto::RuntimeEvent,
    ) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent { seq, event }
    }

    fn replay_kinds(buffer: &RuntimeReplayBuffer) -> Vec<String> {
        buffer
            .replay()
            .into_iter()
            .map(|event| event.event.kind().to_string())
            .collect()
    }

    fn snapshot(messages: Vec<domain::display::HistoryItem>) -> domain::events::LoadedSession {
        snapshot_with_title(None, messages)
    }

    fn snapshot_with_title(
        title: Option<String>,
        messages: Vec<domain::display::HistoryItem>,
    ) -> domain::events::LoadedSession {
        domain::events::LoadedSession {
            session_id: "s1".to_string(),
            provider: "main".to_string(),
            model: "test-model".to_string(),
            thinking_effort: None,
            active_profile: domain::events::ActiveProfile::Main,
            title,
            messages,
            subagents: Vec::new(),
            usage: domain::events::SessionUsageSnapshot::default(),
        }
    }

    fn persisted_message(
        session_id: &str,
        role: &str,
        blocks: Vec<domain::message::ContentBlock>,
    ) -> runtime_contract::RuntimePersistenceEvent {
        runtime_contract::RuntimePersistenceEvent::InsertMessage {
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

        // title 变化现在由 server 走自己的事件通道,replay buffer 收到的是协议层
        // `TypedRuntimeEvent::SessionTitleChanged`,不再包成 `RuntimeToServerEvent`。
        buffer.record(sequenced_runtime_event(
            1,
            session_title_changed_protocol_event(Some("old".to_string())),
        ));
        buffer.record(SequencedRuntimeEvent {
            seq: 2,
            event: thinking_display_changed_protocol_event(false),
        });
        buffer.record(runtime_event(
            3,
            runtime_contract::RuntimeToServerEvent::AgentManagementUpdated {
                records: Vec::new(),
            },
        ));
        buffer.record(sequenced_runtime_event(
            4,
            session_title_changed_protocol_event(Some("new".to_string())),
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
            client_proto::TypedRuntimeEvent::SessionTitleChanged(event)
                if event.title.as_deref() == Some("new")
        ));
    }

    #[test]
    fn replay_buffer_drops_session_title_recovered_by_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced_runtime_event(
            1,
            session_title_changed_protocol_event(Some("hello".to_string())),
        ));
        buffer.record_snapshot(
            &snapshot_with_title(Some("hello".to_string()), Vec::new()),
            &[],
        );

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
            runtime_contract::RuntimeToServerEvent::PlanSubmitted(domain::events::SubmittedPlan {
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
            runtime_contract::RuntimeToServerEvent::PlanApprovalResolved {
                plan_id: "plan_1".to_string(),
                action: client_proto::PlanApprovalAction::ContinueDiscussing,
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
        let item = domain::display::HistoryItem::Message(domain::message::Message::from_user_text(
            "hello".to_string(),
        ));
        let event = runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::UserMessageInjected {
                item: item.clone(),
                client_echo_id: Some("echo-1".to_string()),
            },
        )
        .expect("event should encode");

        buffer.record(SequencedRuntimeEvent { seq: 1, event });
        buffer.record_snapshot(&snapshot(vec![item]), &[]);

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
                vec![domain::message::ContentBlock::from_text(
                    "hello".to_string(),
                )],
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
                    domain::message::ContentBlock::from_thinking("thinking".to_string()),
                    domain::message::ContentBlock::from_text("answer".to_string()),
                    domain::message::ContentBlock::from_tool_use(
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
        let assistant = domain::message::Message::new(
            domain::message::Role::Assistant,
            vec![
                domain::message::ContentBlock::from_thinking("thinking".to_string()),
                domain::message::ContentBlock::from_text("answer".to_string()),
                domain::message::ContentBlock::from_tool_use(
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
            event: runtime_event_from_runtime_contract_event(
                runtime_contract::RuntimeToServerEvent::ToolUse(
                    match assistant.content[2].clone() {
                        domain::message::ContentBlock::ToolUse(tool_use) => tool_use,
                        _ => unreachable!(),
                    },
                ),
            )
            .expect("event should encode"),
        });

        // LLM 级去重现在走 jsonl 路径,数据放在新参数 `&[Message]` 里。
        buffer.record_snapshot(&snapshot(Vec::new()), &[assistant]);

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
                vec![domain::message::ContentBlock::from_tool_result(
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
        let tool_result = domain::message::ContentBlock::from_tool_result(
            "tool_1".to_string(),
            false,
            "done".to_string(),
        );
        let domain::message::ContentBlock::ToolResult(tool_result_event) = tool_result.clone()
        else {
            unreachable!();
        };

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(SequencedRuntimeEvent {
            seq: 3,
            event: runtime_event_from_runtime_contract_event(
                runtime_contract::RuntimeToServerEvent::ToolResult(tool_result_event),
            )
            .expect("event should encode"),
        });
        // LLM 级去重现在走 jsonl 路径,数据放在新参数 `&[Message]` 里。
        let tool_result_message =
            domain::message::Message::new(domain::message::Role::User, vec![tool_result]);
        buffer.record_snapshot(&snapshot(Vec::new()), &[tool_result_message]);

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
            client_proto::TypedRuntimeEvent::TextDelta(event) if event.delta == "second"
        ));
    }

    #[test]
    fn replay_buffer_replays_in_progress_compact_tail_without_run() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(runtime_event(
            1,
            runtime_contract::RuntimeToServerEvent::CompactSummaryStarted(
                domain::events::CompactEvent {
                    trigger: domain::events::CompactTrigger::Manual,
                    session_id: Some("s1".to_string()),
                    agent_label: None,
                },
            ),
        ));
        buffer.record(runtime_event(
            2,
            runtime_contract::RuntimeToServerEvent::CompactSummaryDelta(
                domain::events::CompactSummaryDeltaEvent {
                    trigger: domain::events::CompactTrigger::Manual,
                    delta: "partial".to_string(),
                    session_id: Some("s1".to_string()),
                    agent_label: None,
                },
            ),
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
            client_proto::TypedRuntimeEvent::CompactSummaryDelta(event) if event.delta == "partial"
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
