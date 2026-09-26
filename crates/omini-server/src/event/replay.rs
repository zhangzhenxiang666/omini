use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use std::collections::HashMap;

pub const MAX_AGENT_STREAM_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_TASK_OUTPUT_SNAPSHOT_BYTES: usize = 256 * 1024;
const MAX_TASK_REPLAY_COUNT: usize = 30;

#[derive(Clone)]
pub struct SequencedRuntimeEvent {
    // seq 只在单个 ThreadRuntime 内单调递增，用来让 WebSocket replay 和订阅流去重。
    pub seq: u64,
    pub event: client_proto::RuntimeEvent,
}

/// 保存重连时必须补发、但尚未被 snapshot/status 覆盖的运行中尾部和少量 UI 状态。
#[derive(Default)]
pub struct RuntimeReplayBuffer {
    // run_started 之前的用户注入事件先暂存，确保重连客户端能看到刚提交的输入。
    pending_prefix: Vec<SequencedRuntimeEvent>,
    // run_started 是 replay 的锚点；run 结束或 thread snapshot 后会清空。
    run_started: Option<SequencedRuntimeEvent>,
    // 当前 turn 尚未被持久化 snapshot 覆盖的尾部增量。
    current_tail: Vec<SequencedRuntimeEvent>,
    // compact 不一定发生在 query run 内；单独保留它的流式尾部供新连接补齐。
    compact_started: Option<SequencedRuntimeEvent>,
    compact_tail: Vec<SequencedRuntimeEvent>,
    // 计划确认发生在 run_finished 后，不能依赖 run tail；保留到任一客户端完成确认。
    pending_plan_approval: Option<SequencedRuntimeEvent>,
    // server/core 的轻量 UI 状态事件不一定落在消息 snapshot 中；每类只保留最新值。
    latest_thread_title: Option<SequencedRuntimeEvent>,
    latest_agent_management: Option<SequencedRuntimeEvent>,
    // Agent task 流不依赖前台运行生命周期，并按 task ID 相互隔离。
    agent_streams: HashMap<String, AgentStreamReplay>,
    // 通用任务状态和 Bash 输出同样跨 run 生命周期保留，供同一 server 进程内重连。
    task_streams: HashMap<String, TaskStreamReplay>,
}

#[derive(Default)]
struct AgentStreamReplay {
    events: Vec<SequencedRuntimeEvent>,
    delta_bytes: usize,
    truncated: bool,
}

#[derive(Default)]
struct TaskStreamReplay {
    changed: Option<SequencedRuntimeEvent>,
    outputs: Vec<SequencedRuntimeEvent>,
    output_bytes: HashMap<domain::task::TaskOutputStream, usize>,
}

impl RuntimeReplayBuffer {
    pub fn record(&mut self, event: SequencedRuntimeEvent) {
        match &event.event.event {
            client_proto::TypedRuntimeEvent::TaskChanged(changed) => {
                let task_id = changed.task.task_id.clone();
                self.task_streams.entry(task_id).or_default().changed = Some(event);
                self.prune_task_streams();
                return;
            }
            client_proto::TypedRuntimeEvent::TaskOutputDelta(output) => {
                let task_id = output.task_id.clone();
                let stream = output.stream;
                let replay = self.task_streams.entry(task_id).or_default();
                let size = output.delta.len();
                replay
                    .output_bytes
                    .entry(stream)
                    .and_modify(|bytes| *bytes = bytes.saturating_add(size))
                    .or_insert(size);
                replay.outputs.push(event);
                truncate_task_output(replay, stream);
                self.prune_task_streams();
                return;
            }
            _ => {}
        }
        if let client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) = &event.event.event {
            self.record_agent_event(envelope.task_id.clone(), event);
            return;
        }
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
            "thread_title_changed" => {
                self.latest_thread_title = Some(event);
            }
            "agent_management_updated" => {
                self.latest_agent_management = Some(event);
            }
            "thread_snapshot" => {
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
                + usize::from(self.latest_thread_title.is_some())
                + usize::from(self.latest_agent_management.is_some())
                + self
                    .agent_streams
                    .values()
                    .map(|stream| stream.events.len())
                    .sum::<usize>()
                + self
                    .task_streams
                    .values()
                    .map(|stream| stream.outputs.len() + usize::from(stream.changed.is_some()))
                    .sum::<usize>(),
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
        if let Some(event) = &self.latest_thread_title {
            replay.push(event.clone());
        }
        if let Some(event) = &self.latest_agent_management {
            replay.push(event.clone());
        }
        for stream in self.agent_streams.values() {
            replay.extend(stream.events.iter().cloned());
        }
        for stream in self.task_streams.values() {
            replay.extend(stream.changed.iter().cloned());
            replay.extend(stream.outputs.iter().cloned());
        }
        replay.sort_by_key(|event| event.seq);
        replay
    }

    pub fn record_persistence(
        &mut self,
        owner_thread_id: &str,
        event: &runtime_contract::RuntimePersistenceEvent,
    ) {
        // 持久化成功意味着对应 UI 片段下一次会从 snapshot 恢复，应从 replay 中裁掉。
        match event {
            runtime_contract::RuntimePersistenceEvent::UiMessageAppended {
                thread_id,
                message,
                ..
            } if thread_id == owner_thread_id => {
                if message.role == omini_model::message::Role::Assistant {
                    self.drop_current_assistant_tail();
                } else if message
                    .content
                    .iter()
                    .any(omini_model::message::ContentBlock::is_tool_result)
                {
                    self.drop_persisted_tool_results();
                } else {
                    self.drop_pending_user_injection();
                }
            }
            runtime_contract::RuntimePersistenceEvent::InsertCompactSummaryMessage {
                thread_id,
                ..
            } if thread_id == owner_thread_id => {
                self.drop_current_compact_summary_tail();
            }
            _ => {}
        }
    }

    pub fn record_snapshot(
        &mut self,
        snapshot: &runtime_contract::thread_domain::LoadedThread,
        thread_messages: &[omini_model::message::Message],
    ) {
        // 新连接发 snapshot 前再做一次裁剪，覆盖持久化事件和 snapshot 生成之间的竞态。
        self.drop_user_injections_in_snapshot(snapshot);
        self.drop_thread_title_in_snapshot(snapshot);
        // LLM 级去重使用当前 context version 的 `llm_messages`，不使用 UI 集合。
        if self.current_assistant_tail_is_in_snapshot(thread_messages) {
            self.drop_current_assistant_tail();
        }
        if self.current_tool_results_are_in_snapshot(thread_messages) {
            self.drop_persisted_tool_results();
        }
    }

    fn clear(&mut self) {
        self.pending_prefix.clear();
        self.run_started = None;
        self.current_tail.clear();
        self.clear_compact_tail();
        self.pending_plan_approval = None;
        // Agent task 流拥有独立生命周期，收到 RunFinished 时仍需保留。
    }

    fn record_agent_event(&mut self, task_id: String, event: SequencedRuntimeEvent) {
        let payload = match &event.event.event {
            client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) => envelope.payload.clone(),
            _ => return,
        };
        if matches!(
            payload,
            runtime_contract::thread_domain::AgentTaskEvent::Finished { .. }
        ) {
            self.agent_streams.remove(&task_id);
            return;
        }
        let stream = self.agent_streams.entry(task_id).or_default();
        match payload {
            runtime_contract::thread_domain::AgentTaskEvent::Started { .. } => {
                stream.events.clear();
                stream.delta_bytes = 0;
                stream.truncated = false;
                stream.events.push(event);
            }
            runtime_contract::thread_domain::AgentTaskEvent::TurnStarted => {
                stream.events.retain(|entry| {
                    matches!(
                        &entry.event.event,
                        client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope)
                            if matches!(envelope.payload, runtime_contract::thread_domain::AgentTaskEvent::Started { .. })
                    )
                });
                stream.delta_bytes = 0;
                stream.truncated = false;
                stream.events.push(event);
            }
            runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { delta } => {
                push_agent_delta(stream, event, delta, true);
            }
            runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta } => {
                push_agent_delta(stream, event, delta, false);
            }
            runtime_contract::thread_domain::AgentTaskEvent::MessageCommitted {
                message, ..
            } => {
                if message.role == omini_model::message::Role::Assistant {
                    stream.events.retain(|entry| {
                        !matches!(
                            &entry.event.event,
                            client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope)
                                if matches!(
                                    envelope.payload,
                                    runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { .. }
                                        | runtime_contract::thread_domain::AgentTaskEvent::TextDelta { .. }
                                        | runtime_contract::thread_domain::AgentTaskEvent::ToolUse { .. }
                                )
                        )
                    });
                } else if message
                    .content
                    .iter()
                    .any(omini_model::message::ContentBlock::is_tool_result)
                {
                    stream.events.retain(|entry| {
                        !matches!(
                            &entry.event.event,
                            client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope)
                                if matches!(envelope.payload, runtime_contract::thread_domain::AgentTaskEvent::ToolResult { .. })
                        )
                    });
                }
                stream.delta_bytes = agent_delta_bytes(&stream.events);
            }
            runtime_contract::thread_domain::AgentTaskEvent::ToolUse { .. }
            | runtime_contract::thread_domain::AgentTaskEvent::ToolResult { .. }
            | runtime_contract::thread_domain::AgentTaskEvent::TurnEnded => {
                stream.events.push(event)
            }
            runtime_contract::thread_domain::AgentTaskEvent::Finished { .. } => unreachable!(),
        }
    }

    fn prune_task_streams(&mut self) {
        while self.task_streams.len() > MAX_TASK_REPLAY_COUNT {
            let oldest = self
                .task_streams
                .iter()
                .min_by_key(|(_, replay)| {
                    replay.changed.as_ref().map_or_else(
                        || replay.outputs.first().map_or(0, |event| event.seq),
                        |event| event.seq,
                    )
                })
                .map(|(task_id, _)| task_id.clone());
            let Some(oldest) = oldest else { break };
            self.task_streams.remove(&oldest);
        }
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

    fn drop_user_injections_in_snapshot(
        &mut self,
        snapshot: &runtime_contract::thread_domain::LoadedThread,
    ) {
        self.pending_prefix
            .retain(|event| !user_injection_is_in_snapshot(event, snapshot));
        self.current_tail
            .retain(|event| !user_injection_is_in_snapshot(event, snapshot));
    }

    fn drop_thread_title_in_snapshot(
        &mut self,
        snapshot: &runtime_contract::thread_domain::LoadedThread,
    ) {
        let Some(event) = &self.latest_thread_title else {
            return;
        };
        if thread_title_payload(&event.event) == Some(snapshot.title.as_ref()) {
            self.latest_thread_title = None;
        }
    }

    fn current_assistant_tail_is_in_snapshot(
        &self,
        thread_messages: &[omini_model::message::Message],
    ) -> bool {
        let blocks = assistant_tail_blocks(&self.current_tail);
        !blocks.is_empty()
            && thread_messages.iter().any(|message| {
                message.role == omini_model::message::Role::Assistant
                    && strip_thinking_durations(&message.content) == blocks
            })
    }

    fn current_tool_results_are_in_snapshot(
        &self,
        thread_messages: &[omini_model::message::Message],
    ) -> bool {
        let blocks = tool_result_tail_blocks(&self.current_tail);
        !blocks.is_empty()
            && thread_messages.iter().any(|message| {
                message.role == omini_model::message::Role::User && message.content == blocks
            })
    }
}

fn truncate_task_output(replay: &mut TaskStreamReplay, stream: domain::task::TaskOutputStream) {
    let mut excess = replay
        .output_bytes
        .get(&stream)
        .copied()
        .unwrap_or_default()
        .saturating_sub(MAX_TASK_OUTPUT_SNAPSHOT_BYTES);
    if excess == 0 {
        return;
    }
    for event in &mut replay.outputs {
        if excess == 0 {
            break;
        }
        let client_proto::TypedRuntimeEvent::TaskOutputDelta(output) = &mut event.event.event
        else {
            continue;
        };
        if output.stream != stream {
            continue;
        }
        let mut boundary = excess.min(output.delta.len());
        while boundary < output.delta.len() && !output.delta.is_char_boundary(boundary) {
            boundary += 1;
        }
        output.delta.drain(..boundary);
        excess = excess.saturating_sub(boundary);
    }
    replay.outputs.retain(|event| {
        matches!(
            &event.event.event,
            client_proto::TypedRuntimeEvent::TaskOutputDelta(output) if !output.delta.is_empty()
        )
    });
    replay
        .output_bytes
        .insert(stream, MAX_TASK_OUTPUT_SNAPSHOT_BYTES);
}

/// 判断待 replay 的用户注入事件是否已经出现在持久化 snapshot 中。
fn user_injection_is_in_snapshot(
    event: &SequencedRuntimeEvent,
    snapshot: &runtime_contract::thread_domain::LoadedThread,
) -> bool {
    let client_proto::TypedRuntimeEvent::UserMessageInjected { item, .. } = &event.event.event
    else {
        return false;
    };
    let item = crate::conversation::domain_entry(item.clone());
    snapshot.messages.iter().any(|message| message == &item)
}

fn thread_title_payload(event: &client_proto::RuntimeEvent) -> Option<Option<&String>> {
    let client_proto::TypedRuntimeEvent::ThreadTitleChanged(event) = &event.event else {
        return None;
    };
    Some(event.title.as_ref())
}

/// 把当前 assistant 流式尾部重组为完整内容块，供 snapshot 去重比较。
fn assistant_tail_blocks(
    events: &[SequencedRuntimeEvent],
) -> Vec<omini_model::message::ContentBlock> {
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
                blocks.push(omini_model::message::ContentBlock::ToolUse(
                    tool_use.clone(),
                ));
            }
            _ => {}
        }
    }
    blocks
}

/// 收集当前尾部中尚未被 snapshot 覆盖的工具结果块。
fn tool_result_tail_blocks(
    events: &[SequencedRuntimeEvent],
) -> Vec<omini_model::message::ContentBlock> {
    events
        .iter()
        .filter_map(|event| match &event.event.event {
            client_proto::TypedRuntimeEvent::ToolResult(tool_result) => Some(
                omini_model::message::ContentBlock::ToolResult(tool_result.clone()),
            ),
            _ => None,
        })
        .collect()
}

/// 剥离 Thinking 块的思考时长后再比较。
/// delta 重建的尾部块不带时长（时长由 engine 在 turn 结束时写入持久化消息），
/// 全等比较前需先剥离该显示元数据，否则去重失效会导致重连时重放已持久化内容。
fn strip_thinking_durations(
    content: &[omini_model::message::ContentBlock],
) -> Vec<omini_model::message::ContentBlock> {
    let mut content = content.to_vec();
    for block in &mut content {
        if let omini_model::message::ContentBlock::Thinking(thinking) = block {
            thinking.duration_ms = None;
        }
    }
    content
}

/// 将连续文本或 thinking delta 合并成可比较的 `ContentBlock`。
fn push_delta_block(
    blocks: &mut Vec<omini_model::message::ContentBlock>,
    delta: &str,
    thinking: bool,
) {
    match (thinking, blocks.last_mut()) {
        (true, Some(omini_model::message::ContentBlock::Thinking(block))) => {
            block.thinking.push_str(delta)
        }
        (false, Some(omini_model::message::ContentBlock::Text(block))) => {
            block.text.push_str(delta)
        }
        (true, _) => blocks.push(omini_model::message::ContentBlock::from_thinking(
            delta.to_string(),
        )),
        (false, _) => blocks.push(omini_model::message::ContentBlock::from_text(
            delta.to_string(),
        )),
    }
}

fn push_agent_delta(
    stream: &mut AgentStreamReplay,
    event: SequencedRuntimeEvent,
    delta: String,
    thinking: bool,
) {
    let merged = stream.events.last_mut().is_some_and(|last| {
        let client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) = &mut last.event.event
        else {
            return false;
        };
        match (&mut envelope.payload, thinking) {
            (
                runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { delta: current },
                true,
            )
            | (
                runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta: current },
                false,
            ) => {
                current.push_str(&delta);
                true
            }
            _ => false,
        }
    });
    if !merged {
        stream.events.push(event);
    }
    stream.delta_bytes = stream.delta_bytes.saturating_add(delta.len());
    truncate_agent_stream(stream);
}

fn truncate_agent_stream(stream: &mut AgentStreamReplay) {
    let mut excess = stream
        .delta_bytes
        .saturating_sub(MAX_AGENT_STREAM_SNAPSHOT_BYTES);
    if excess == 0 {
        return;
    }
    stream.truncated = true;
    for event in &mut stream.events {
        if excess == 0 {
            break;
        }
        let client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) = &mut event.event.event
        else {
            continue;
        };
        let delta = match &mut envelope.payload {
            runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { delta }
            | runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta } => delta,
            _ => continue,
        };
        if delta.len() <= excess {
            excess -= delta.len();
            delta.clear();
        } else {
            let mut boundary = excess;
            while boundary < delta.len() && !delta.is_char_boundary(boundary) {
                boundary += 1;
            }
            delta.drain(..boundary);
            excess = 0;
        }
    }
    stream.events.retain(|event| {
        let client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) = &event.event.event else {
            return true;
        };
        match &envelope.payload {
            runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { delta }
            | runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta } => {
                !delta.is_empty()
            }
            _ => true,
        }
    });
    stream.delta_bytes = agent_delta_bytes(&stream.events);
    if let Some(envelope) = stream.events.iter_mut().find_map(|event| {
        let client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) = &mut event.event.event
        else {
            return None;
        };
        matches!(
            envelope.payload,
            runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { .. }
                | runtime_contract::thread_domain::AgentTaskEvent::TextDelta { .. }
        )
        .then_some(envelope)
    }) {
        envelope.truncated = true;
    }
}

fn agent_delta_bytes(events: &[SequencedRuntimeEvent]) -> usize {
    events
        .iter()
        .filter_map(|event| {
            let client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) = &event.event.event
            else {
                return None;
            };
            match &envelope.payload {
                runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { delta }
                | runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta } => {
                    Some(delta.len())
                }
                _ => None,
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::event::bridge::{
        runtime_event_from_runtime_contract_event, thread_title_changed_protocol_event,
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

    fn agent_event(
        seq: u64,
        task_id: &str,
        payload: runtime_contract::thread_domain::AgentTaskEvent,
    ) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: client_proto::RuntimeEvent::new(
                client_proto::TypedRuntimeEvent::AgentTaskEvent(
                    runtime_contract::thread_domain::AgentTaskEventEnvelope {
                        task_id: task_id.to_string(),
                        thread_id: format!("thread_{task_id}"),
                        parent_task_id: None,
                        owner_thread_id: "owner".to_string(),
                        truncated: false,
                        payload,
                    },
                ),
            ),
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
                item: crate::conversation::history_item_from_model_message(
                    omini_model::message::Message::from_user_text("hello".to_string()),
                ),
                client_echo_id: None,
            },
            "run_started" => client_proto::TypedRuntimeEvent::RunStarted,
            "run_finished" => client_proto::TypedRuntimeEvent::RunFinished,
            "turn_started" => client_proto::TypedRuntimeEvent::TurnStarted,
            "turn_ended" => client_proto::TypedRuntimeEvent::TurnEnded,
            "tool_use" => {
                client_proto::TypedRuntimeEvent::ToolUse(omini_model::message::ToolUseBlock {
                    id: "tool_1".to_string(),
                    name: "read".to_string(),
                    input: HashMap::new(),
                })
            }
            "tool_result" => {
                client_proto::TypedRuntimeEvent::ToolResult(omini_model::message::ToolResultBlock {
                    tool_use_id: "tool_1".to_string(),
                    is_error: false,
                    content: "done".to_string(),
                    metadata: None,
                })
            }
            "tool_pause_requested" => client_proto::TypedRuntimeEvent::ToolPauseRequested(
                runtime_contract::thread_domain::ToolPauseRequest {
                    tool_use_id: "tool_1".to_string(),
                    preview_tool_use_id: None,
                    tool_name: "bash".to_string(),
                    permission_source: None,
                    source_thread_id: None,
                    source_agent_label: None,
                    kind: runtime_contract::thread_domain::ToolPauseKind::Permission(
                        runtime_contract::thread_domain::PermissionPreview::Custom {
                            tool_name: "bash".to_string(),
                            payload: serde_json::Map::new(),
                        },
                    ),
                },
            ),
            "thread_snapshot" => {
                client_proto::TypedRuntimeEvent::ThreadSnapshot(client_proto::ThreadSnapshotEvent {
                    thread_id: "s1".to_string(),
                    messages: Vec::new(),
                    agent_tasks: Vec::new(),
                    usage: runtime_contract::thread_domain::ThreadUsageSnapshot::default(),
                })
            }
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

    fn snapshot(
        messages: Vec<domain::conversation::ConversationEntry>,
    ) -> runtime_contract::thread_domain::LoadedThread {
        snapshot_with_title(None, messages)
    }

    fn snapshot_with_title(
        title: Option<String>,
        messages: Vec<domain::conversation::ConversationEntry>,
    ) -> runtime_contract::thread_domain::LoadedThread {
        runtime_contract::thread_domain::LoadedThread {
            thread_id: "s1".to_string(),
            provider: "main".to_string(),
            model: "test-model".to_string(),
            thinking_effort: None,
            active_profile: runtime_contract::thread_domain::ActiveProfile::Main,
            title,
            messages,
            agent_tasks: Vec::new(),
            usage: runtime_contract::thread_domain::ThreadUsageSnapshot::default(),
        }
    }

    fn persisted_message(
        thread_id: &str,
        role: omini_model::message::Role,
        blocks: Vec<omini_model::message::ContentBlock>,
    ) -> runtime_contract::RuntimePersistenceEvent {
        runtime_contract::RuntimePersistenceEvent::UiMessageAppended {
            thread_id: thread_id.to_string(),
            message: omini_model::message::Message::new(role.clone(), blocks),
            model_ref: (role == omini_model::message::Role::Assistant)
                .then(|| "test/model".to_string()),
        }
    }

    fn fixed_time() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 0, 0, 0)
            .single()
            .expect("fixed test time should be valid")
    }

    #[test]
    fn replay_buffer_ignores_idle_runtime_events() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "notification"));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_restores_task_status_and_bounded_output_after_run_end() {
        let mut buffer = RuntimeReplayBuffer::default();
        let task = domain::task::TaskInfo {
            task_id: "bash_tool_1".to_string(),
            owner_thread_id: "owner".to_string(),
            kind: domain::task::TaskKind::Bash,
            title: "cargo check".to_string(),
            status: domain::task::TaskStatus::Running,
            created_at: fixed_time(),
            updated_at: fixed_time(),
            completed_at: None,
            result_summary: None,
        };
        buffer.record(runtime_event(
            1,
            runtime_contract::RuntimeToServerEvent::TaskChanged(domain::task::TaskChangedEvent {
                task,
            }),
        ));
        buffer.record(runtime_event(
            2,
            runtime_contract::RuntimeToServerEvent::TaskOutputDelta(
                domain::task::TaskOutputDelta {
                    task_id: "bash_tool_1".to_string(),
                    tool_use_id: "bash_tool_1".to_string(),
                    stream: domain::task::TaskOutputStream::Stdout,
                    delta: "progress".to_string(),
                },
            ),
        ));
        buffer.record(sequenced(3, "run_finished"));

        let replay = buffer.replay();
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].event.kind(), "task_changed");
        assert_eq!(replay[1].event.kind(), "task_output_delta");
    }

    #[test]
    fn replay_buffer_replays_latest_server_local_state_events() {
        let mut buffer = RuntimeReplayBuffer::default();

        // title 变化现在由 server 走自己的事件通道,replay buffer 收到的是协议层
        // `TypedRuntimeEvent::ThreadTitleChanged`,不再包成 `RuntimeToServerEvent`。
        buffer.record(sequenced_runtime_event(
            1,
            thread_title_changed_protocol_event(Some("old".to_string())),
        ));
        buffer.record(runtime_event(
            2,
            runtime_contract::RuntimeToServerEvent::AgentManagementUpdated {
                records: Vec::new(),
            },
        ));
        buffer.record(sequenced_runtime_event(
            3,
            thread_title_changed_protocol_event(Some("new".to_string())),
        ));

        let replay = buffer.replay();

        assert_eq!(
            replay.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(
            replay
                .iter()
                .map(|event| event.event.kind())
                .collect::<Vec<_>>(),
            vec!["agent_management_updated", "thread_title_changed"]
        );
        assert!(matches!(
            &replay[1].event.event,
            client_proto::TypedRuntimeEvent::ThreadTitleChanged(event)
                if event.title.as_deref() == Some("new")
        ));
    }

    #[test]
    fn replay_buffer_drops_thread_title_recovered_by_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced_runtime_event(
            1,
            thread_title_changed_protocol_event(Some("hello".to_string())),
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
            runtime_contract::RuntimeToServerEvent::PlanSubmitted(
                runtime_contract::thread_domain::SubmittedPlan {
                    id: "plan".to_string(),
                    title: "Plan".to_string(),
                    markdown: "# Plan".to_string(),
                    path: PathBuf::new(),
                    created_at: fixed_time(),
                },
            ),
        ));

        assert_eq!(replay_kinds(&buffer), vec!["plan_submitted"]);

        buffer.record(runtime_event(
            2,
            runtime_contract::RuntimeToServerEvent::PlanApprovalResolved {
                plan_id: "plan".to_string(),
                action: client_proto::PlanApprovalAction::ContinueDiscussing,
            },
        ));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn replay_buffer_preserves_pending_user_until_run_starts() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "user_message_injected"));
        buffer.record(sequenced(2, "thread_snapshot"));
        buffer.record(sequenced(3, "run_started"));

        assert_eq!(
            replay_kinds(&buffer),
            vec!["user_message_injected", "run_started"]
        );
    }

    #[test]
    fn replay_buffer_drops_user_injection_found_in_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();
        let input = domain::conversation::UserInput {
            intent: domain::input::UserInputIntent::Message,
            parts: vec![domain::input::InputPart::Text {
                text: "hello".to_string(),
            }],
            attachments: Vec::new(),
        };
        let item = domain::conversation::ConversationEntry::UserInput(input.clone());
        let event =
            client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::UserMessageInjected {
                item: client_proto::HistoryItem::UserInput(input),
                client_echo_id: Some("echo-1".to_string()),
            });

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
                omini_model::message::Role::User,
                vec![omini_model::message::ContentBlock::from_text(
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
                omini_model::message::Role::Assistant,
                vec![
                    omini_model::message::ContentBlock::from_thinking("thinking".to_string()),
                    omini_model::message::ContentBlock::from_text("answer".to_string()),
                    omini_model::message::ContentBlock::from_tool_use(
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
        let assistant = omini_model::message::Message::new(
            omini_model::message::Role::Assistant,
            vec![
                omini_model::message::ContentBlock::from_thinking("thinking".to_string()),
                omini_model::message::ContentBlock::from_text("answer".to_string()),
                omini_model::message::ContentBlock::from_tool_use(
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
                        omini_model::message::ContentBlock::ToolUse(tool_use) => tool_use,
                        _ => unreachable!(),
                    },
                ),
            )
            .expect("event should encode"),
        });

        // LLM 级去重使用单独传入的当前 context 消息。
        buffer.record_snapshot(&snapshot(Vec::new()), &[assistant]);

        assert_eq!(replay_kinds(&buffer), vec!["run_started", "turn_started"]);
    }

    #[test]
    fn replay_buffer_drops_assistant_tail_with_thinking_duration_in_snapshot() {
        // 持久化消息的 thinking 块带 engine 测量的时长，而 delta 重建块没有；
        // 去重比较必须剥离时长，否则重连会重放已持久化的思考/正文 delta。
        let assistant = omini_model::message::Message::new(
            omini_model::message::Role::Assistant,
            vec![
                omini_model::message::ContentBlock::Thinking(omini_model::message::ThinkingBlock {
                    thinking: "thinking".to_string(),
                    duration_ms: Some(5300),
                }),
                omini_model::message::ContentBlock::from_text("answer".to_string()),
            ],
        );
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "thinking_delta", "thinking"));
        buffer.record(delta(4, "text_delta", "answer"));

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
                omini_model::message::Role::User,
                vec![omini_model::message::ContentBlock::from_tool_result(
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
        let tool_result = omini_model::message::ContentBlock::from_tool_result(
            "tool_1".to_string(),
            false,
            "done".to_string(),
        );
        let omini_model::message::ContentBlock::ToolResult(tool_result_event) = tool_result.clone()
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
        // LLM 级去重使用单独传入的当前 context 消息。
        let tool_result_message =
            omini_model::message::Message::new(omini_model::message::Role::User, vec![tool_result]);
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
                runtime_contract::thread_domain::CompactEvent {
                    trigger: runtime_contract::thread_domain::CompactTrigger::Manual,
                    thread_id: Some("s1".to_string()),
                    agent_label: None,
                },
            ),
        ));
        buffer.record(runtime_event(
            2,
            runtime_contract::RuntimeToServerEvent::CompactSummaryDelta(
                runtime_contract::thread_domain::CompactSummaryDeltaEvent {
                    trigger: runtime_contract::thread_domain::CompactTrigger::Manual,
                    delta: "partial".to_string(),
                    thread_id: Some("s1".to_string()),
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
    fn replay_buffer_clears_active_run_on_thread_snapshot() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "text_delta", "hello"));
        buffer.record(sequenced(4, "thread_snapshot"));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn agent_stream_survives_parent_run_finished_and_tasks_stay_isolated() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(agent_event(
            1,
            "one",
            runtime_contract::thread_domain::AgentTaskEvent::TextDelta {
                delta: "first".to_string(),
            },
        ));
        buffer.record(agent_event(
            2,
            "two",
            runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta {
                delta: "second".to_string(),
            },
        ));
        buffer.record(sequenced(3, "run_finished"));

        let replay = buffer.replay();
        assert_eq!(replay.len(), 2);
        assert!(matches!(
            &replay[0].event.event,
            client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope)
                if envelope.task_id == "one"
                    && matches!(
                        &envelope.payload,
                        runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta } if delta == "first"
                    )
        ));
        assert!(matches!(
            &replay[1].event.event,
            client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope)
                if envelope.task_id == "two"
                    && matches!(
                        &envelope.payload,
                        runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { delta } if delta == "second"
                    )
        ));
    }

    #[test]
    fn agent_stream_merges_deltas_and_committed_message_trims_them() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(agent_event(
            1,
            "one",
            runtime_contract::thread_domain::AgentTaskEvent::TextDelta {
                delta: "hel".to_string(),
            },
        ));
        buffer.record(agent_event(
            2,
            "one",
            runtime_contract::thread_domain::AgentTaskEvent::TextDelta {
                delta: "lo".to_string(),
            },
        ));

        let replay = buffer.replay();
        assert_eq!(replay.len(), 1);
        assert!(matches!(
            &replay[0].event.event,
            client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope)
                if matches!(
                    &envelope.payload,
                    runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta } if delta == "hello"
                )
        ));

        buffer.record(agent_event(
            3,
            "one",
            runtime_contract::thread_domain::AgentTaskEvent::MessageCommitted {
                message: omini_model::message::Message::new(
                    omini_model::message::Role::Assistant,
                    vec![omini_model::message::ContentBlock::from_text(
                        "hello".to_string(),
                    )],
                ),
                persist_llm_history: true,
            },
        ));
        assert!(buffer.replay().iter().all(|entry| !matches!(
            &entry.event.event,
            client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope)
                if matches!(
                    envelope.payload,
                    runtime_contract::thread_domain::AgentTaskEvent::TextDelta { .. }
                        | runtime_contract::thread_domain::AgentTaskEvent::ThinkingDelta { .. }
                )
        )));
    }

    #[test]
    fn agent_stream_keeps_utf8_safe_tail_with_truncation_marker() {
        let mut buffer = RuntimeReplayBuffer::default();
        let text = "界".repeat(MAX_AGENT_STREAM_SNAPSHOT_BYTES);

        buffer.record(agent_event(
            1,
            "one",
            runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta: text },
        ));

        let replay = buffer.replay();
        assert_eq!(replay.len(), 1);
        let client_proto::TypedRuntimeEvent::AgentTaskEvent(envelope) = &replay[0].event.event
        else {
            panic!("expected agent task event");
        };
        let runtime_contract::thread_domain::AgentTaskEvent::TextDelta { delta } =
            &envelope.payload
        else {
            panic!("expected text delta");
        };
        assert!(envelope.truncated);
        assert!(delta.len() <= MAX_AGENT_STREAM_SNAPSHOT_BYTES);
        assert!(std::str::from_utf8(delta.as_bytes()).is_ok());
    }
}
