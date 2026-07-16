use crate::event::bridge::{
    session_runtime_skills_from_runtime_snapshot, tool_pause_event_kind_from_request,
};
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
    source_session_id: Option<String>,
    source_agent_label: Option<String>,
}

impl RuntimeToolActivity {
    fn to_protocol(&self, now: DateTime<Utc>) -> client_proto::SessionRuntimeTool {
        client_proto::SessionRuntimeTool {
            tool_use_id: self.tool_use_id.clone(),
            tool_name: self.tool_name.clone(),
            started_at: self.started_at,
            elapsed_ms: elapsed_ms(self.started_at, now),
            source_session_id: self.source_session_id.clone(),
            source_agent_label: self.source_agent_label.clone(),
        }
    }
}

/// 当前仍在等待客户端处理的暂停请求投影。
#[derive(Debug, Clone)]
struct RuntimePendingPause {
    tool_use_id: String,
    tool_name: String,
    kind: client_proto::ToolPauseEventKind,
    source_session_id: Option<String>,
    source_agent_label: Option<String>,
}

impl RuntimePendingPause {
    fn to_protocol(&self) -> client_proto::SessionRuntimePendingPause {
        client_proto::SessionRuntimePendingPause {
            tool_use_id: self.tool_use_id.clone(),
            tool_name: self.tool_name.clone(),
            kind: self.kind,
            source_session_id: self.source_session_id.clone(),
            source_agent_label: self.source_agent_label.clone(),
        }
    }
}

/// 当前运行中子 agent 的来源信息，用于标注其工具活动。
#[derive(Debug, Clone)]
struct RuntimeSubagentContext {
    agent_label: String,
}

/// 从 runtime 事件流增量推导出的会话运行态。
#[derive(Debug, Default)]
pub struct RuntimeStatusProjection {
    // active profile 不落入持久化消息；新连接只能从运行态投影拿到当前值。
    active_profile: domain::events::ActiveProfile,
    query_started_at: Option<DateTime<Utc>>,
    compact_started_at: Option<DateTime<Utc>>,
    query_pause_started_at: Option<DateTime<Utc>>,
    query_paused_ms: u64,
    query_state: client_proto::SessionRuntimeState,
    pending_pauses: HashMap<String, RuntimePendingPause>,
    pending_plan_approval: Option<client_proto::PlanSubmittedEvent>,
    active_tools: HashMap<String, RuntimeToolActivity>,
    subagents: HashMap<String, RuntimeSubagentContext>,
}

/// 生成协议状态快照时由 session 层补充的外部上下文。
pub struct RuntimeStatusSnapshotContext {
    pub loaded: bool,
    pub controller_id: Option<String>,
    pub connected_client_count: usize,
    pub skills: Vec<runtime_contract::session::RuntimeSkillSnapshot>,
    // Core 只暴露 MCP 运行态快照；wire DTO 投影由 server 边界负责。
    pub mcp_servers: Vec<runtime_contract::mcp::RuntimeMcpServerSnapshot>,
    pub subagent_sessions: Vec<client_proto::AgentSummary>,
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
            client_proto::TypedRuntimeEvent::SessionTitleChanged(_)
            | client_proto::TypedRuntimeEvent::ThinkingDisplayChanged(_)
            | client_proto::TypedRuntimeEvent::AgentManagementUpdated { .. } => {}
            // 和 TUI 标签语义保持一致：run/turn 刚开始先显示 Thinking，直到可见输出或工具开始。
            client_proto::TypedRuntimeEvent::RunStarted => self.start_query(now),
            client_proto::TypedRuntimeEvent::RunFinished
            | client_proto::TypedRuntimeEvent::SessionSnapshot(_) => self.clear_active_run(),
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
            client_proto::TypedRuntimeEvent::SubagentStarted(event) => {
                self.record_subagent_started(event, now)
            }
            client_proto::TypedRuntimeEvent::SubagentToolUse(event) => {
                self.record_subagent_tool_use(event, now)
            }
            client_proto::TypedRuntimeEvent::SubagentToolResult(event) => {
                self.record_subagent_tool_result(event, now)
            }
            client_proto::TypedRuntimeEvent::SubagentFinished(event) => {
                self.record_subagent_finished(event, now)
            }
            _ => {}
        }
    }

    pub fn to_protocol(
        &self,
        session_id: &str,
        context: RuntimeStatusSnapshotContext,
    ) -> client_proto::SessionRuntimeStatus {
        let mut pending_pauses = self
            .pending_pauses
            .values()
            .map(RuntimePendingPause::to_protocol)
            .collect::<Vec<_>>();
        pending_pauses.sort_by(|left, right| left.tool_use_id.cmp(&right.tool_use_id));

        let mut active_tools = self
            .active_tools
            .values()
            .map(|tool| tool.to_protocol(context.now))
            .collect::<Vec<_>>();
        active_tools.sort_by(|left, right| left.tool_use_id.cmp(&right.tool_use_id));

        client_proto::SessionRuntimeStatus {
            session_id: session_id.to_string(),
            state: self.state(),
            active_profile: self.active_profile,
            loaded: context.loaded,
            controller_id: context.controller_id,
            connected_client_count: context.connected_client_count,
            activity: self.activity(context.now),
            pending_pauses,
            pending_plan_approval: self.pending_plan_approval.clone(),
            active_tools,
            skills: session_runtime_skills_from_runtime_snapshot(context.skills),
            mcp_servers: mcp_servers_to_protocol(context.mcp_servers),
            subagent_sessions: context.subagent_sessions,
            git_branch: context.git_branch,
        }
    }

    fn start_query(&mut self, now: DateTime<Utc>) {
        self.query_started_at = Some(now);
        self.compact_started_at = None;
        self.query_pause_started_at = None;
        self.query_paused_ms = 0;
        self.query_state = client_proto::SessionRuntimeState::Thinking;
        self.pending_pauses.clear();
        self.pending_plan_approval = None;
        self.active_tools.clear();
        self.subagents.clear();
    }

    fn clear_active_run(&mut self) {
        self.query_started_at = None;
        self.compact_started_at = None;
        self.query_pause_started_at = None;
        self.query_paused_ms = 0;
        self.query_state = client_proto::SessionRuntimeState::Idle;
        self.pending_pauses.clear();
        self.pending_plan_approval = None;
        self.active_tools.clear();
        self.subagents.clear();
    }

    fn mark_query_working(&mut self) {
        if self.query_started_at.is_some() {
            self.query_state = client_proto::SessionRuntimeState::Working;
        }
    }

    fn mark_query_thinking(&mut self) {
        if self.query_started_at.is_some() {
            self.query_state = client_proto::SessionRuntimeState::Thinking;
        }
    }

    fn record_tool_use(
        &mut self,
        tool_use: &client_proto::ToolUseBlock,
        now: DateTime<Utc>,
        source_session_id: Option<String>,
        source_agent_label: Option<String>,
    ) {
        self.record_tool(
            &tool_use.id,
            &tool_use.name,
            now,
            source_session_id,
            source_agent_label,
        );
        self.mark_query_working();
    }

    fn record_tool(
        &mut self,
        tool_use_id: &str,
        tool_name: &str,
        now: DateTime<Utc>,
        source_session_id: Option<String>,
        source_agent_label: Option<String>,
    ) -> RuntimeToolActivity {
        let tool = RuntimeToolActivity {
            tool_use_id: tool_use_id.to_string(),
            tool_name: tool_name.to_string(),
            started_at: now,
            source_session_id,
            source_agent_label,
        };
        self.active_tools
            .insert(tool_use_id.to_string(), tool.clone());
        tool
    }

    fn finish_tool(&mut self, tool_use_id: &str) {
        self.active_tools.remove(tool_use_id);
    }

    fn record_tool_pause(
        &mut self,
        request: &client_proto::ToolPauseRequestedEvent,
        now: DateTime<Utc>,
    ) {
        if self.query_started_at.is_some()
            && self.pending_pauses.is_empty()
            && self.query_pause_started_at.is_none()
        {
            self.query_pause_started_at = Some(now);
        }
        self.pending_pauses.insert(
            request.tool_use_id.clone(),
            RuntimePendingPause {
                tool_use_id: request.tool_use_id.clone(),
                tool_name: request.tool_name.clone(),
                kind: tool_pause_event_kind_from_request(request),
                source_session_id: request.source_session_id.clone(),
                source_agent_label: request.source_agent_label.clone(),
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

    fn record_subagent_started(
        &mut self,
        event: &client_proto::SubagentStartedEvent,
        _now: DateTime<Utc>,
    ) {
        self.subagents.insert(
            event.session_id.clone(),
            RuntimeSubagentContext {
                agent_label: event.agent_label.clone(),
            },
        );
        self.mark_query_working();
    }

    fn record_subagent_tool_use(
        &mut self,
        event: &client_proto::SubagentToolUseEvent,
        now: DateTime<Utc>,
    ) {
        let agent_label = self
            .subagents
            .get(&event.session_id)
            .map(|subagent| subagent.agent_label.clone());
        self.record_tool_use_for_subagent(&event.tool_use, now, &event.session_id, agent_label);
        self.mark_query_working();
    }

    fn record_tool_use_for_subagent(
        &mut self,
        tool_use: &client_proto::ToolUseBlock,
        now: DateTime<Utc>,
        session_id: &str,
        agent_label: Option<String>,
    ) -> Option<RuntimeToolActivity> {
        Some(self.record_tool(
            &tool_use.id,
            &tool_use.name,
            now,
            Some(session_id.to_string()),
            agent_label,
        ))
    }

    fn record_subagent_tool_result(
        &mut self,
        event: &client_proto::SubagentToolResultEvent,
        now: DateTime<Utc>,
    ) {
        let tool_use_id = &event.tool_result.tool_use_id;
        self.finish_tool(tool_use_id);
        self.finish_pause(tool_use_id, now);
        self.finish_pause(&format!("{}:{tool_use_id}", event.session_id), now);
        self.mark_query_working();
    }

    fn record_subagent_finished(
        &mut self,
        event: &client_proto::SubagentFinishedEvent,
        _now: DateTime<Utc>,
    ) {
        self.subagents.remove(&event.session_id);
        self.mark_query_working();
    }

    pub fn state(&self) -> client_proto::SessionRuntimeState {
        if self.compact_started_at.is_some() {
            client_proto::SessionRuntimeState::Compacting
        } else if !self.pending_pauses.is_empty() || self.pending_plan_approval.is_some() {
            client_proto::SessionRuntimeState::Waiting
        } else if self.query_started_at.is_some() {
            self.query_state
        } else {
            client_proto::SessionRuntimeState::Idle
        }
    }

    fn activity(&self, now: DateTime<Utc>) -> Option<client_proto::SessionRuntimeActivity> {
        if let Some(started_at) = self.compact_started_at {
            Some(client_proto::SessionRuntimeActivity {
                kind: client_proto::SessionRuntimeActivityKind::Compact,
                started_at,
                elapsed_ms: elapsed_ms(started_at, now),
            })
        } else {
            self.query_started_at
                .map(|started_at| client_proto::SessionRuntimeActivity {
                    kind: client_proto::SessionRuntimeActivityKind::Query,
                    started_at,
                    elapsed_ms: self.query_elapsed_ms(started_at, now),
                })
        }
    }

    pub fn active_profile(&self) -> domain::events::ActiveProfile {
        self.active_profile
    }

    /// 返回当前仍在运行中的子代理 session_id 集合，供 snapshot 恢复真实状态用。
    pub fn running_subagent_session_ids(&self) -> Vec<String> {
        self.subagents.keys().cloned().collect()
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
) -> Vec<client_proto::SessionRuntimeMcpServer> {
    snapshots
        .into_iter()
        .map(runtime_mcp_server_to_protocol)
        .collect()
}

fn runtime_mcp_server_to_protocol(
    snapshot: runtime_contract::mcp::RuntimeMcpServerSnapshot,
) -> client_proto::SessionRuntimeMcpServer {
    client_proto::SessionRuntimeMcpServer {
        name: snapshot.name,
        status: runtime_mcp_server_status_to_protocol(snapshot.status),
        last_error: snapshot.last_error,
        tools: snapshot
            .tools
            .into_iter()
            .map(|tool| client_proto::SessionRuntimeMcpTool {
                name: tool.name,
                registered_name: tool.registered_name,
                description: tool.description,
            })
            .collect(),
    }
}

fn runtime_mcp_server_status_to_protocol(
    status: runtime_contract::mcp::RuntimeMcpServerStatus,
) -> client_proto::SessionRuntimeMcpStatus {
    match status {
        runtime_contract::mcp::RuntimeMcpServerStatus::Disabled => {
            client_proto::SessionRuntimeMcpStatus::Disabled
        }
        runtime_contract::mcp::RuntimeMcpServerStatus::Connecting => {
            client_proto::SessionRuntimeMcpStatus::Connecting
        }
        runtime_contract::mcp::RuntimeMcpServerStatus::Ready => {
            client_proto::SessionRuntimeMcpStatus::Ready
        }
        runtime_contract::mcp::RuntimeMcpServerStatus::Failed => {
            client_proto::SessionRuntimeMcpStatus::Failed
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
            "session_snapshot" => client_proto::TypedRuntimeEvent::SessionSnapshot(
                client_proto::SessionSnapshotEvent {
                    session_id: Some("s1".to_string()),
                    messages: Vec::new(),
                    subagents: Vec::new(),
                    usage: client_proto::SessionUsageSnapshot::default(),
                },
            ),
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
                source_session_id: None,
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
    ) -> client_proto::SessionRuntimeStatus {
        projection.to_protocol(
            "s1",
            RuntimeStatusSnapshotContext {
                loaded: true,
                controller_id: Some("client_1".to_string()),
                connected_client_count: 1,
                skills: Vec::new(),
                mcp_servers: Vec::new(),
                subagent_sessions: Vec::new(),
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
                    id: "plan_1".to_string(),
                    title: "Plan".to_string(),
                    markdown: "# Plan".to_string(),
                    path: PathBuf::new(),
                    created_at: now,
                },
            )),
            now,
        );
        let status = status_snapshot(&projection, now);
        assert_eq!(status.state, client_proto::SessionRuntimeState::Waiting);
        assert_eq!(
            status
                .pending_plan_approval
                .as_ref()
                .map(|plan| plan.plan_id.as_str()),
            Some("plan_1")
        );

        projection.record_event(
            &client_proto::RuntimeEvent::new(
                client_proto::TypedRuntimeEvent::PlanApprovalResolved(
                    client_proto::PlanApprovalResolvedEvent {
                        plan_id: "plan_1".to_string(),
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
        assert_eq!(status.state, client_proto::SessionRuntimeState::Thinking);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.kind),
            Some(client_proto::SessionRuntimeActivityKind::Query)
        );
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(42)
        );

        projection.record_event(&sequenced(2, "turn_started").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            client_proto::SessionRuntimeState::Thinking
        );

        projection.record_event(&delta(3, "thinking_delta", "hmm").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            client_proto::SessionRuntimeState::Thinking
        );

        projection.record_event(&delta(4, "text_delta", "hello").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            client_proto::SessionRuntimeState::Working
        );

        projection.record_event(
            &tool_pause_event("tool_1"),
            started_at + chrono::Duration::milliseconds(50),
        );
        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(70));
        assert_eq!(status.state, client_proto::SessionRuntimeState::Waiting);
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
        assert_eq!(status.state, client_proto::SessionRuntimeState::Working);
        assert!(status.pending_pauses.is_empty());
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(80)
        );

        projection.record_event(&sequenced(3, "run_finished").event, started_at);
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.state, client_proto::SessionRuntimeState::Idle);
        assert!(status.activity.is_none());
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
        assert_eq!(status.state, client_proto::SessionRuntimeState::Waiting);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(10)
        );

        projection.record_event(
            &tool_result_event("tool_1"),
            started_at + chrono::Duration::milliseconds(60),
        );
        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(70));
        assert_eq!(status.state, client_proto::SessionRuntimeState::Waiting);
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
        assert_eq!(status.state, client_proto::SessionRuntimeState::Working);
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
                        session_id: Some("s1".to_string()),
                        agent_label: None,
                    },
                ),
            ),
            started_at,
        );

        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(7));
        assert_eq!(status.state, client_proto::SessionRuntimeState::Compacting);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.kind),
            Some(client_proto::SessionRuntimeActivityKind::Compact)
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
                        session_id: Some("s1".to_string()),
                        agent_label: None,
                    },
                ),
            ),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.state, client_proto::SessionRuntimeState::Idle);
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
                skills: vec![runtime_contract::session::RuntimeSkillSnapshot {
                    name: "writer".to_string(),
                    description: "Write carefully".to_string(),
                    short_description: None,
                    source_kind: runtime_contract::session::RuntimeSkillSourceKind::Project,
                    directory: "/repo/.omini/skills/writer".into(),
                    status: runtime_contract::session::RuntimeCapabilityStatus::Available,
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
                subagent_sessions: vec![client_proto::AgentSummary {
                    name: "explorer".to_string(),
                    description: "Read-only exploration agent.".to_string(),
                    short_description: None,
                    location: "<built-in>".to_string(),
                }],
                now,
                git_branch: None,
            },
        );

        assert_eq!(status.state, client_proto::SessionRuntimeState::Idle);
        assert_eq!(status.skills.len(), 1);
        assert_eq!(
            status.skills[0].source_kind,
            client_proto::SkillSourceKind::Project
        );
        assert_eq!(status.mcp_servers.len(), 1);
        assert_eq!(
            status.mcp_servers[0].status,
            client_proto::SessionRuntimeMcpStatus::Ready
        );
        assert_eq!(
            status.mcp_servers[0].tools[0].registered_name,
            "mcp__docs__search"
        );
        assert_eq!(status.subagent_sessions.len(), 1);
        assert_eq!(status.subagent_sessions[0].name, "explorer");
    }

    #[test]
    fn runtime_status_tracks_subagent_tools_through_active_tools() {
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
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::SubagentStarted(
                client_proto::SubagentStartedEvent {
                    session_id: "sub_1".to_string(),
                    parent_session_id: "s1".to_string(),
                    spawn_tool_use_id: "tool_subagent".to_string(),
                    agent_label: "explorer".to_string(),
                },
            )),
            started_at,
        );
        projection.record_event(
            &client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::SubagentToolUse(
                client_proto::SubagentToolUseEvent {
                    session_id: "sub_1".to_string(),
                    tool_use: ToolUseBlock {
                        id: "sub_tool_1".to_string(),
                        name: "read".to_string(),
                        input: HashMap::new(),
                    },
                },
            )),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert!(status.subagent_sessions.is_empty());
        let subagent_tool = status
            .active_tools
            .iter()
            .find(|tool| tool.tool_use_id == "sub_tool_1")
            .expect("subagent tool should be tracked as an active tool");
        assert_eq!(subagent_tool.source_session_id.as_deref(), Some("sub_1"));
        assert_eq!(
            subagent_tool.source_agent_label.as_deref(),
            Some("explorer")
        );
    }
}
