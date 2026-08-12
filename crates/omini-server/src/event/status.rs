use crate::event::bridge::thread_runtime_skills_from_runtime_snapshot;
use chrono::{DateTime, Utc};
use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct RuntimeToolActivity {
    tool_use_id: String,
    tool_name: String,
    started_at: DateTime<Utc>,
    source_thread_id: Option<String>,
    source_agent_label: Option<String>,
}

impl RuntimeToolActivity {
    fn to_protocol(&self, now: DateTime<Utc>) -> client_proto::ThreadRuntimeTool {
        client_proto::ThreadRuntimeTool {
            tool_use_id: self.tool_use_id.clone(),
            tool_name: self.tool_name.clone(),
            started_at: self.started_at,
            elapsed_ms: elapsed_ms(self.started_at, now),
            source_thread_id: self.source_thread_id.clone(),
            source_agent_label: self.source_agent_label.clone(),
        }
    }
}

/// 当前仍在等待客户端处理的暂停请求投影。
#[derive(Debug, Clone)]
struct RuntimePendingPause {
    request: client_proto::ToolPauseRequest,
    sequence: u64,
}

impl RuntimePendingPause {
    fn to_protocol(&self) -> client_proto::ToolPauseRequest {
        self.request.clone()
    }
}

/// 当前运行中子 agent 的来源信息，用于标注其工具活动。
#[derive(Debug, Clone)]
struct RuntimeAgentTaskContext {
    agent_label: String,
}

/// 从 runtime 事件流增量推导出的线程运行态。
#[derive(Debug, Default)]
pub struct RuntimeStatusProjection {
    // active profile 不落入持久化消息；新连接只能从运行态投影拿到当前值。
    active_profile: domain::events::ActiveProfile,
    query_started_at: Option<DateTime<Utc>>,
    compact_started_at: Option<DateTime<Utc>>,
    query_pause_started_at: Option<DateTime<Utc>>,
    query_paused_ms: u64,
    query_state: client_proto::ThreadRuntimeState,
    pending_pauses: HashMap<String, RuntimePendingPause>,
    next_pause_sequence: u64,
    pending_plan_approval: Option<client_proto::PlanSubmittedEvent>,
    active_tools: HashMap<String, RuntimeToolActivity>,
    agent_tasks: HashMap<String, RuntimeAgentTaskContext>,
}

/// 生成协议状态快照时由 thread 层补充的外部上下文。
pub struct RuntimeStatusSnapshotContext {
    pub loaded: bool,
    pub controller_id: Option<String>,
    pub connected_client_count: usize,
    pub skills: Vec<runtime_contract::thread::RuntimeSkillSnapshot>,
    // Core 只暴露 MCP 运行态快照；wire DTO 投影由 server 边界负责。
    pub mcp_servers: Vec<runtime_contract::mcp::RuntimeMcpServerSnapshot>,
    pub subagent_threads: Vec<client_proto::AgentSummary>,
    pub now: DateTime<Utc>,
    pub git_branch: Option<String>,
}

impl RuntimeStatusProjection {
    pub fn with_active_profile(active_profile: domain::events::ActiveProfile) -> Self {
        Self {
            active_profile,
            ..Self::default()
        }
    }

    pub fn record_event(&mut self, event: &omini_protocol::RuntimeEvent, now: DateTime<Utc>) {
        match &event.event {
            client_proto::TypedRuntimeEvent::ActiveProfileChanged(event) => {
                self.active_profile = event.profile;
            }
            client_proto::TypedRuntimeEvent::ThreadTitleChanged(_)
            | client_proto::TypedRuntimeEvent::ThinkingDisplayChanged(_)
            | client_proto::TypedRuntimeEvent::AgentManagementUpdated { .. } => {}
            // 和 TUI 标签语义保持一致：run/turn 刚开始先显示 Thinking，直到可见输出或工具开始。
            client_proto::TypedRuntimeEvent::RunStarted => self.start_query(now),
            client_proto::TypedRuntimeEvent::RunFinished
            | client_proto::TypedRuntimeEvent::ThreadSnapshot(_) => self.clear_active_run(),
            client_proto::TypedRuntimeEvent::TurnStarted => self.mark_query_thinking(),
            client_proto::TypedRuntimeEvent::TextDelta(_) => self.mark_query_working(),
            client_proto::TypedRuntimeEvent::ThinkingDelta(_) => self.mark_query_thinking(),
            client_proto::TypedRuntimeEvent::ToolUse(tool_use) => {
                self.record_tool_use(tool_use, now, None, None)
            }
            client_proto::TypedRuntimeEvent::ToolResult(tool_result) => {
                self.finish_tool(&tool_result.tool_use_id);
                self.finish_pause(&tool_result.tool_use_id, now);
                self.mark_query_working();
            }
            client_proto::TypedRuntimeEvent::ToolPauseRequested(request) => {
                self.record_tool_pause(request, now)
            }
            client_proto::TypedRuntimeEvent::PlanSubmitted(_) => {
                self.pending_plan_approval = plan_submitted_payload(event);
            }
            client_proto::TypedRuntimeEvent::PlanApprovalResolved(_) => {
                if self.pending_plan_matches(event) {
                    self.pending_plan_approval = None;
                }
            }
            client_proto::TypedRuntimeEvent::CompactSummaryStarted(_) => {
                self.compact_started_at = Some(now);
            }
            client_proto::TypedRuntimeEvent::CompactSummaryFinished(_)
            | client_proto::TypedRuntimeEvent::CompactSummaryFailed(_) => {
                self.compact_started_at = None;
                self.mark_query_working();
            }
            client_proto::TypedRuntimeEvent::AgentTaskEvent(event) => {
                self.record_agent_task_event(event, now)
            }
            _ => {}
        }
    }

    pub fn to_protocol(
        &self,
        thread_id: &str,
        context: RuntimeStatusSnapshotContext,
    ) -> client_proto::ThreadRuntimeStatus {
        let mut pending_pauses = self.pending_pauses.values().collect::<Vec<_>>();
        pending_pauses.sort_by_key(|pause| pause.sequence);
        let pending_pauses = pending_pauses
            .into_iter()
            .map(RuntimePendingPause::to_protocol)
            .collect::<Vec<_>>();

        let mut active_tools = self
            .active_tools
            .values()
            .map(|tool| tool.to_protocol(context.now))
            .collect::<Vec<_>>();
        active_tools.sort_by(|left, right| left.tool_use_id.cmp(&right.tool_use_id));

        client_proto::ThreadRuntimeStatus {
            thread_id: thread_id.to_string(),
            state: self.state(),
            active_profile: self.active_profile,
            loaded: context.loaded,
            controller_id: context.controller_id,
            connected_client_count: context.connected_client_count,
            activity: self.activity(context.now),
            pending_pauses,
            pending_plan_approval: self.pending_plan_approval.clone(),
            active_tools,
            skills: thread_runtime_skills_from_runtime_snapshot(context.skills),
            mcp_servers: mcp_servers_to_protocol(context.mcp_servers),
            subagent_threads: context.subagent_threads,
            git_branch: context.git_branch,
        }
    }

    fn start_query(&mut self, now: DateTime<Utc>) {
        self.query_started_at = Some(now);
        self.compact_started_at = None;
        self.query_pause_started_at = None;
        self.query_paused_ms = 0;
        self.query_state = client_proto::ThreadRuntimeState::Thinking;
        self.pending_pauses
            .retain(|_, pause| pause.request.source_thread_id.is_some());
        self.pending_plan_approval = None;
        self.active_tools
            .retain(|_, tool| tool.source_thread_id.is_some());
    }

    fn clear_active_run(&mut self) {
        self.query_started_at = None;
        self.compact_started_at = None;
        self.query_pause_started_at = None;
        self.query_paused_ms = 0;
        self.query_state = client_proto::ThreadRuntimeState::Idle;
        self.pending_pauses
            .retain(|_, pause| pause.request.source_thread_id.is_some());
        self.pending_plan_approval = None;
        self.active_tools
            .retain(|_, tool| tool.source_thread_id.is_some());
    }

    fn mark_query_working(&mut self) {
        if self.query_started_at.is_some() {
            self.query_state = client_proto::ThreadRuntimeState::Working;
        }
    }

    fn mark_query_thinking(&mut self) {
        if self.query_started_at.is_some() {
            self.query_state = client_proto::ThreadRuntimeState::Thinking;
        }
    }

    fn record_tool_use(
        &mut self,
        tool_use: &client_proto::ToolUseBlock,
        now: DateTime<Utc>,
        source_thread_id: Option<String>,
        source_agent_label: Option<String>,
    ) {
        self.record_tool(
            &tool_use.id,
            &tool_use.name,
            now,
            source_thread_id,
            source_agent_label,
        );
        self.mark_query_working();
    }

    fn record_tool(
        &mut self,
        tool_use_id: &str,
        tool_name: &str,
        now: DateTime<Utc>,
        source_thread_id: Option<String>,
        source_agent_label: Option<String>,
    ) -> RuntimeToolActivity {
        let tool = RuntimeToolActivity {
            tool_use_id: tool_use_id.to_string(),
            tool_name: tool_name.to_string(),
            started_at: now,
            source_thread_id,
            source_agent_label,
        };
        self.active_tools
            .insert(tool_use_id.to_string(), tool.clone());
        tool
    }

    fn record_agent_tool(
        &mut self,
        thread_id: &str,
        tool_use: &client_proto::ToolUseBlock,
        now: DateTime<Utc>,
        agent_label: Option<String>,
    ) {
        let activity_key = format!("{thread_id}:{}", tool_use.id);
        let tool = RuntimeToolActivity {
            tool_use_id: tool_use.id.clone(),
            tool_name: tool_use.name.clone(),
            started_at: now,
            source_thread_id: Some(thread_id.to_string()),
            source_agent_label: agent_label,
        };
        self.active_tools.insert(activity_key, tool);
    }

    fn finish_tool(&mut self, tool_use_id: &str) {
        self.active_tools.remove(tool_use_id);
    }

    fn record_tool_pause(&mut self, request: &client_proto::ToolPauseRequest, now: DateTime<Utc>) {
        if self.query_started_at.is_some()
            && self.pending_pauses.is_empty()
            && self.query_pause_started_at.is_none()
        {
            self.query_pause_started_at = Some(now);
        }
        let sequence = self
            .pending_pauses
            .get(&request.tool_use_id)
            .map(|pause| pause.sequence)
            .unwrap_or_else(|| {
                let sequence = self.next_pause_sequence;
                self.next_pause_sequence = self.next_pause_sequence.saturating_add(1);
                sequence
            });
        self.pending_pauses.insert(
            request.tool_use_id.clone(),
            RuntimePendingPause {
                request: request.clone(),
                sequence,
            },
        );
    }

    fn finish_pause(&mut self, tool_use_id: &str, now: DateTime<Utc>) {
        let removed = self.pending_pauses.remove(tool_use_id).is_some();
        if removed && self.pending_pauses.is_empty() {
            self.resume_query_timer(now);
        }
    }

    fn resume_query_timer(&mut self, now: DateTime<Utc>) {
        let Some(paused_at) = self.query_pause_started_at.take() else {
            return;
        };
        self.query_paused_ms = self
            .query_paused_ms
            .saturating_add(elapsed_ms(paused_at, now));
    }

    fn record_agent_task_event(
        &mut self,
        event: &client_proto::AgentTaskEventEnvelope,
        now: DateTime<Utc>,
    ) {
        match &event.payload {
            client_proto::AgentTaskEvent::Started { agent, .. } => {
                self.agent_tasks.insert(
                    event.task_id.clone(),
                    RuntimeAgentTaskContext {
                        agent_label: agent.clone(),
                    },
                );
            }
            client_proto::AgentTaskEvent::ToolUse { tool_use } => {
                let agent_label = self
                    .agent_tasks
                    .get(&event.task_id)
                    .map(|task| task.agent_label.clone());
                self.record_agent_tool(&event.thread_id, tool_use, now, agent_label);
            }
            client_proto::AgentTaskEvent::ToolResult { tool_result } => {
                let tool_use_id = &tool_result.tool_use_id;
                let activity_key = format!("{}:{tool_use_id}", event.thread_id);
                self.finish_tool(&activity_key);
                self.finish_pause(&activity_key, now);
            }
            client_proto::AgentTaskEvent::Finished { .. } => {
                self.agent_tasks.remove(&event.task_id);
                self.active_tools
                    .retain(|_, tool| tool.source_thread_id.as_deref() != Some(&event.thread_id));
                let had_pending_pauses = !self.pending_pauses.is_empty();
                self.pending_pauses.retain(|_, pause| {
                    pause.request.source_thread_id.as_deref() != Some(&event.thread_id)
                });
                if had_pending_pauses && self.pending_pauses.is_empty() {
                    self.resume_query_timer(now);
                }
            }
            client_proto::AgentTaskEvent::TurnStarted
            | client_proto::AgentTaskEvent::ThinkingDelta { .. }
            | client_proto::AgentTaskEvent::TextDelta { .. }
            | client_proto::AgentTaskEvent::MessageCommitted { .. }
            | client_proto::AgentTaskEvent::TurnEnded => {}
        }
    }

    pub fn state(&self) -> client_proto::ThreadRuntimeState {
        if self.compact_started_at.is_some() {
            client_proto::ThreadRuntimeState::Compacting
        } else if !self.pending_pauses.is_empty() || self.pending_plan_approval.is_some() {
            client_proto::ThreadRuntimeState::Waiting
        } else if self.query_started_at.is_some() {
            self.query_state
        } else {
            client_proto::ThreadRuntimeState::Idle
        }
    }

    fn activity(&self, now: DateTime<Utc>) -> Option<client_proto::ThreadRuntimeActivity> {
        if let Some(started_at) = self.compact_started_at {
            Some(client_proto::ThreadRuntimeActivity {
                kind: client_proto::ThreadRuntimeActivityKind::Compact,
                started_at,
                elapsed_ms: elapsed_ms(started_at, now),
            })
        } else {
            self.query_started_at
                .map(|started_at| client_proto::ThreadRuntimeActivity {
                    kind: client_proto::ThreadRuntimeActivityKind::Query,
                    started_at,
                    elapsed_ms: self.query_elapsed_ms(started_at, now),
                })
        }
    }

    pub fn active_profile(&self) -> domain::events::ActiveProfile {
        self.active_profile
    }

    /// 返回当前仍在运行中的子代理 thread_id 集合，供 snapshot 恢复真实状态用。
    pub fn has_active_agent_tasks(&self) -> bool {
        !self.agent_tasks.is_empty()
    }

    fn pending_plan_matches(&self, event: &client_proto::RuntimeEvent) -> bool {
        let Some(pending) = &self.pending_plan_approval else {
            return false;
        };
        plan_approval_resolved_plan_id(event)
            .map(|plan_id| plan_id == pending.plan_id)
            .unwrap_or(true)
    }

    fn query_elapsed_ms(&self, started_at: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
        let active_pause_ms = self
            .query_pause_started_at
            .map(|paused_at| elapsed_ms(paused_at, now))
            .unwrap_or(0);
        elapsed_ms(started_at, now)
            .saturating_sub(self.query_paused_ms.saturating_add(active_pause_ms))
    }
}

// MCP status projection 留在 daemon 边界，避免 core 为运行态能力快照构造协议 DTO。
fn mcp_servers_to_protocol(
    snapshots: Vec<runtime_contract::mcp::RuntimeMcpServerSnapshot>,
) -> Vec<client_proto::ThreadRuntimeMcpServer> {
    snapshots
        .into_iter()
        .map(runtime_mcp_server_to_protocol)
        .collect()
}

fn runtime_mcp_server_to_protocol(
    snapshot: runtime_contract::mcp::RuntimeMcpServerSnapshot,
) -> client_proto::ThreadRuntimeMcpServer {
    client_proto::ThreadRuntimeMcpServer {
        name: snapshot.name,
        status: runtime_mcp_server_status_to_protocol(snapshot.status),
        last_error: snapshot.last_error,
        tools: snapshot
            .tools
            .into_iter()
            .map(|tool| client_proto::ThreadRuntimeMcpTool {
                name: tool.name,
                registered_name: tool.registered_name,
                description: tool.description,
            })
            .collect(),
    }
}

fn runtime_mcp_server_status_to_protocol(
    status: runtime_contract::mcp::RuntimeMcpServerStatus,
) -> client_proto::ThreadRuntimeMcpStatus {
    match status {
        runtime_contract::mcp::RuntimeMcpServerStatus::Disabled => {
            client_proto::ThreadRuntimeMcpStatus::Disabled
        }
        runtime_contract::mcp::RuntimeMcpServerStatus::Connecting => {
            client_proto::ThreadRuntimeMcpStatus::Connecting
        }
        runtime_contract::mcp::RuntimeMcpServerStatus::Ready => {
            client_proto::ThreadRuntimeMcpStatus::Ready
        }
        runtime_contract::mcp::RuntimeMcpServerStatus::Failed => {
            client_proto::ThreadRuntimeMcpStatus::Failed
        }
    }
}

fn elapsed_ms(started_at: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    now.signed_duration_since(started_at)
        .num_milliseconds()
        .max(0) as u64
}

pub fn plan_submitted_payload(
    event: &client_proto::RuntimeEvent,
) -> Option<client_proto::PlanSubmittedEvent> {
    if let client_proto::TypedRuntimeEvent::PlanSubmitted(plan) = &event.event {
        Some(client_proto::PlanSubmittedEvent {
            plan_id: plan.id.clone(),
            title: plan.title.clone(),
            markdown: plan.markdown.clone(),
        })
    } else {
        None
    }
}

pub fn plan_approval_resolved_plan_id(event: &client_proto::RuntimeEvent) -> Option<String> {
    if let client_proto::TypedRuntimeEvent::PlanApprovalResolved(resolved) = &event.event {
        Some(resolved.plan_id.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::event::replay::SequencedRuntimeEvent;
    use omini_domain::events as event_types;
    use omini_domain::message::{ToolResultBlock, ToolUseBlock};
    use omini_runtime_contract::mcp::RuntimeMcpToolSnapshot;

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
                _ => panic!("unsupported delta test event kind: {kind}"),
            }),
        }
    }

    fn typed_test_event(kind: &str) -> client_proto::RuntimeEvent {
        client_proto::RuntimeEvent::new(match kind {
            "run_started" => client_proto::TypedRuntimeEvent::RunStarted,
            "run_finished" => client_proto::TypedRuntimeEvent::RunFinished,
            "turn_started" => client_proto::TypedRuntimeEvent::TurnStarted,
            "thread_snapshot" => {
                client_proto::TypedRuntimeEvent::ThreadSnapshot(client_proto::ThreadSnapshotEvent {
                    thread_id: "s1".to_string(),
                    messages: Vec::new(),
                    agent_tasks: Vec::new(),
                    usage: client_proto::ThreadUsageSnapshot::default(),
                })
            }
            _ => panic!("unsupported test event kind: {kind}"),
        })
    }

    fn tool_pause_event(tool_use_id: &str) -> client_proto::RuntimeEvent {
        client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::ToolPauseRequested(
            event_types::ToolPauseRequest {
                tool_use_id: tool_use_id.to_string(),
                preview_tool_use_id: None,
                tool_name: "bash".to_string(),
                permission_source: None,
                source_thread_id: None,
                source_agent_label: None,
                kind: event_types::ToolPauseKind::Permission(
                    event_types::PermissionPreview::Custom {
                        tool_name: "bash".to_string(),
                        payload: serde_json::Map::new(),
                    },
                ),
            },
        ))
    }

    fn tool_result_event(tool_use_id: &str) -> client_proto::RuntimeEvent {
        client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::ToolResult(
            ToolResultBlock {
                tool_use_id: tool_use_id.to_string(),
                is_error: false,
                content: "done".to_string(),
                metadata: None,
            },
        ))
    }

    fn status_snapshot(
        projection: &RuntimeStatusProjection,
        now: DateTime<Utc>,
    ) -> client_proto::ThreadRuntimeStatus {
        projection.to_protocol(
            "s1",
            RuntimeStatusSnapshotContext {
                loaded: true,
                controller_id: Some("client_1".to_string()),
                connected_client_count: 1,
                skills: Vec::new(),
                mcp_servers: Vec::new(),
                subagent_threads: Vec::new(),
                now,
                git_branch: None,
            },
        )
    }

    #[test]
    fn runtime_status_projection_tracks_active_profile() {
        let mut projection = RuntimeStatusProjection::default();

        projection.record_event(
            &client_proto::RuntimeEvent::new(
                client_proto::TypedRuntimeEvent::ActiveProfileChanged(
                    client_proto::ActiveProfileChangedEvent {
                        profile: domain::events::ActiveProfile::Plan,
                    },
                ),
            ),
            Utc::now(),
        );

        assert_eq!(
            projection.active_profile(),
            domain::events::ActiveProfile::Plan
        );
        assert_eq!(
            status_snapshot(&projection, Utc::now()).active_profile,
            client_proto::ActiveProfile::Plan
        );
    }

    #[test]
    fn runtime_status_projection_tracks_pending_plan_approval() {
        let mut projection = RuntimeStatusProjection::default();
        let now = Utc::now();

        projection.record_event(
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::PlanSubmitted(
                client_proto::SubmittedPlan {
                    id: "plan".to_string(),
                    title: "Plan".to_string(),
                    markdown: "# Plan".to_string(),
                    path: PathBuf::new(),
                    created_at: now,
                },
            )),
            now,
        );
        let status = status_snapshot(&projection, now);
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Waiting);
        assert_eq!(
            status
                .pending_plan_approval
                .as_ref()
                .map(|plan| plan.plan_id.as_str()),
            Some("plan")
        );

        projection.record_event(
            &client_proto::RuntimeEvent::new(
                client_proto::TypedRuntimeEvent::PlanApprovalResolved(
                    client_proto::PlanApprovalResolvedEvent {
                        plan_id: "plan".to_string(),
                        action: client_proto::PlanApprovalAction::ContinueDiscussing,
                    },
                ),
            ),
            now,
        );
        assert!(
            status_snapshot(&projection, now)
                .pending_plan_approval
                .is_none()
        );
    }

    #[test]
    fn runtime_status_tracks_query_state_and_elapsed_time() {
        let mut projection = RuntimeStatusProjection::default();
        let started_at = Utc::now();

        projection.record_event(&sequenced(1, "run_started").event, started_at);
        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(42));
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Thinking);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.kind),
            Some(client_proto::ThreadRuntimeActivityKind::Query)
        );
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(42)
        );

        projection.record_event(&sequenced(2, "turn_started").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            client_proto::ThreadRuntimeState::Thinking
        );

        projection.record_event(&delta(3, "thinking_delta", "hmm").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            client_proto::ThreadRuntimeState::Thinking
        );

        projection.record_event(&delta(4, "text_delta", "hello").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            client_proto::ThreadRuntimeState::Working
        );

        projection.record_event(
            &tool_pause_event("tool_1"),
            started_at + chrono::Duration::milliseconds(50),
        );
        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(70));
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Waiting);
        assert_eq!(status.pending_pauses.len(), 1);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(50)
        );

        projection.record_event(
            &tool_result_event("tool_1"),
            started_at + chrono::Duration::milliseconds(90),
        );
        let status = status_snapshot(
            &projection,
            started_at + chrono::Duration::milliseconds(120),
        );
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Working);
        assert!(status.pending_pauses.is_empty());
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(80)
        );

        projection.record_event(&sequenced(3, "run_finished").event, started_at);
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Idle);
        assert!(status.activity.is_none());
    }

    #[test]
    fn runtime_status_keeps_full_agent_pause_after_parent_run_finishes() {
        let mut projection = RuntimeStatusProjection::default();
        let now = Utc::now();
        let pause = event_types::ToolPauseRequest {
            tool_use_id: "agent_1:tool_1".to_string(),
            preview_tool_use_id: Some("tool_1".to_string()),
            tool_name: "bash".to_string(),
            permission_source: None,
            source_thread_id: Some("agent_1".to_string()),
            source_agent_label: Some("explorer".to_string()),
            kind: event_types::ToolPauseKind::Permission(event_types::PermissionPreview::Custom {
                tool_name: "bash".to_string(),
                payload: serde_json::Map::from_iter([(
                    "command".to_string(),
                    serde_json::Value::String("pwd".to_string()),
                )]),
            }),
        };

        projection.record_event(&sequenced(1, "run_started").event, now);
        projection.record_event(&sequenced(2, "run_finished").event, now);
        projection.record_event(
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::ToolPauseRequested(
                pause.clone(),
            )),
            now,
        );

        let status = status_snapshot(&projection, now);
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Waiting);
        assert_eq!(status.pending_pauses, vec![pause]);

        projection.record_event(
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::AgentTaskEvent(
                client_proto::AgentTaskEventEnvelope {
                    task_id: "task_1".to_string(),
                    thread_id: "agent_1".to_string(),
                    parent_task_id: None,
                    owner_thread_id: "s1".to_string(),
                    truncated: false,
                    payload: client_proto::AgentTaskEvent::Finished {
                        status: client_proto::AgentTaskStatus::Cancelled,
                        result: None,
                    },
                },
            )),
            now,
        );

        assert!(status_snapshot(&projection, now).pending_pauses.is_empty());
    }

    #[test]
    fn runtime_status_resumes_elapsed_after_all_pending_pauses_finish() {
        let mut projection = RuntimeStatusProjection::default();
        let started_at = Utc::now();

        projection.record_event(&sequenced(1, "run_started").event, started_at);
        projection.record_event(
            &tool_pause_event("tool_1"),
            started_at + chrono::Duration::milliseconds(10),
        );
        projection.record_event(
            &tool_pause_event("tool_2"),
            started_at + chrono::Duration::milliseconds(20),
        );

        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(50));
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Waiting);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(10)
        );

        projection.record_event(
            &tool_result_event("tool_1"),
            started_at + chrono::Duration::milliseconds(60),
        );
        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(70));
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Waiting);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(10)
        );

        projection.record_event(
            &tool_result_event("tool_2"),
            started_at + chrono::Duration::milliseconds(80),
        );
        let status = status_snapshot(
            &projection,
            started_at + chrono::Duration::milliseconds(100),
        );
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Working);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(30)
        );
    }

    #[test]
    fn runtime_status_tracks_compact_activity() {
        let mut projection = RuntimeStatusProjection::default();
        let started_at = Utc::now();

        projection.record_event(
            &client_proto::RuntimeEvent::new(
                client_proto::TypedRuntimeEvent::CompactSummaryStarted(
                    client_proto::CompactSummaryStartedEvent {
                        trigger: client_proto::CompactTrigger::Manual,
                        thread_id: Some("s1".to_string()),
                        agent_label: None,
                    },
                ),
            ),
            started_at,
        );

        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(7));
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Compacting);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.kind),
            Some(client_proto::ThreadRuntimeActivityKind::Compact)
        );
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(7)
        );

        projection.record_event(
            &client_proto::RuntimeEvent::new(
                client_proto::TypedRuntimeEvent::CompactSummaryFinished(
                    client_proto::CompactSummaryFinishedEvent {
                        trigger: client_proto::CompactTrigger::Manual,
                        summary: "done".to_string(),
                        after_tokens: 1,
                        thread_id: Some("s1".to_string()),
                        agent_label: None,
                    },
                ),
            ),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.state, client_proto::ThreadRuntimeState::Idle);
        assert!(status.activity.is_none());
    }

    #[test]
    fn runtime_status_includes_capability_snapshots() {
        let projection = RuntimeStatusProjection::default();
        let now = Utc::now();
        let status = projection.to_protocol(
            "s1",
            RuntimeStatusSnapshotContext {
                loaded: true,
                controller_id: None,
                connected_client_count: 0,
                skills: vec![runtime_contract::thread::RuntimeSkillSnapshot {
                    name: "writer".to_string(),
                    description: "Write carefully".to_string(),
                    short_description: None,
                    source_kind: runtime_contract::thread::RuntimeSkillSourceKind::Project,
                    directory: "/repo/.omini/skills/writer".into(),
                    status: runtime_contract::thread::RuntimeCapabilityStatus::Available,
                    disable_model_invocation: false,
                    user_invocable: true,
                }],
                mcp_servers: vec![runtime_contract::mcp::RuntimeMcpServerSnapshot {
                    name: "docs".to_string(),
                    status: runtime_contract::mcp::RuntimeMcpServerStatus::Ready,
                    last_error: None,
                    tools: vec![RuntimeMcpToolSnapshot {
                        name: "search".to_string(),
                        registered_name: "mcp__docs__search".to_string(),
                        description: "Search docs".to_string(),
                    }],
                }],
                subagent_threads: vec![client_proto::AgentSummary {
                    name: "explorer".to_string(),
                    description: "Read-only exploration agent.".to_string(),
                    short_description: None,
                    location: "<built-in>".to_string(),
                }],
                now,
                git_branch: None,
            },
        );

        assert_eq!(status.state, client_proto::ThreadRuntimeState::Idle);
        assert_eq!(status.skills.len(), 1);
        assert_eq!(
            status.skills[0].source_kind,
            client_proto::SkillSourceKind::Project
        );
        assert_eq!(status.mcp_servers.len(), 1);
        assert_eq!(
            status.mcp_servers[0].status,
            client_proto::ThreadRuntimeMcpStatus::Ready
        );
        assert_eq!(
            status.mcp_servers[0].tools[0].registered_name,
            "mcp__docs__search"
        );
        assert_eq!(status.subagent_threads.len(), 1);
        assert_eq!(status.subagent_threads[0].name, "explorer");
    }

    #[test]
    fn runtime_status_tracks_agent_task_tools_through_active_tools() {
        let mut projection = RuntimeStatusProjection::default();
        let started_at = Utc::now();

        projection.record_event(&sequenced(1, "run_started").event, started_at);
        projection.record_event(
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::ToolUse(
                ToolUseBlock {
                    id: "tool_skill".to_string(),
                    name: "skill".to_string(),
                    input: HashMap::from([(
                        "name".to_string(),
                        serde_json::Value::String("rust".to_string()),
                    )]),
                },
            )),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.active_tools.len(), 1);
        assert_eq!(status.active_tools[0].tool_name, "skill");

        projection.record_event(
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::AgentTaskEvent(
                client_proto::AgentTaskEventEnvelope {
                    task_id: "task_1".to_string(),
                    thread_id: "agent_1".to_string(),
                    parent_task_id: None,
                    owner_thread_id: "s1".to_string(),
                    truncated: false,
                    payload: client_proto::AgentTaskEvent::Started {
                        parent_thread_id: "s1".to_string(),
                        spawn_tool_use_id: "tool_agent".to_string(),
                        agent: "explorer".to_string(),
                        title: "Explore".to_string(),
                        depth: 1,
                        execution_mode: client_proto::AgentTaskExecutionMode::Background,
                    },
                },
            )),
            started_at,
        );
        projection.record_event(
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::AgentTaskEvent(
                client_proto::AgentTaskEventEnvelope {
                    task_id: "task_1".to_string(),
                    thread_id: "agent_1".to_string(),
                    parent_task_id: None,
                    owner_thread_id: "s1".to_string(),
                    truncated: false,
                    payload: client_proto::AgentTaskEvent::ToolUse {
                        tool_use: ToolUseBlock {
                            id: "sub_tool_1".to_string(),
                            name: "read".to_string(),
                            input: HashMap::new(),
                        },
                    },
                },
            )),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert!(status.subagent_threads.is_empty());
        let subagent_tool = status
            .active_tools
            .iter()
            .find(|tool| tool.tool_use_id == "sub_tool_1")
            .expect("subagent tool should be tracked as an active tool");
        assert_eq!(subagent_tool.source_thread_id.as_deref(), Some("agent_1"));
        assert_eq!(
            subagent_tool.source_agent_label.as_deref(),
            Some("explorer")
        );
    }
}
