use super::*;

#[derive(Debug, Clone)]
struct RuntimeToolActivity {
    tool_use_id: String,
    tool_name: String,
    started_at: DateTime<Utc>,
    source_session_id: Option<String>,
    source_agent_label: Option<String>,
}

impl RuntimeToolActivity {
    fn to_protocol(&self, now: DateTime<Utc>) -> protocol::SessionRuntimeTool {
        protocol::SessionRuntimeTool {
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
    kind: protocol::ToolPauseEventKind,
    source_session_id: Option<String>,
    source_agent_label: Option<String>,
}

impl RuntimePendingPause {
    fn to_protocol(&self) -> protocol::SessionRuntimePendingPause {
        protocol::SessionRuntimePendingPause {
            tool_use_id: self.tool_use_id.clone(),
            tool_name: self.tool_name.clone(),
            kind: self.kind,
            source_session_id: self.source_session_id.clone(),
            source_agent_label: self.source_agent_label.clone(),
        }
    }
}

/// 当前会话下子 agent 的运行态投影。
#[derive(Debug, Clone)]
struct RuntimeSubagentActivity {
    session_id: String,
    agent_label: String,
    status: protocol::SubagentStatus,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    active_tool: Option<RuntimeToolActivity>,
}

impl RuntimeSubagentActivity {
    fn to_protocol(&self, now: DateTime<Utc>) -> protocol::SessionRuntimeSubagent {
        protocol::SessionRuntimeSubagent {
            session_id: self.session_id.clone(),
            agent_label: self.agent_label.clone(),
            status: self.status,
            started_at: self.started_at,
            finished_at: self.finished_at,
            active_tool: self.active_tool.as_ref().map(|tool| tool.to_protocol(now)),
        }
    }
}

/// 从 runtime 事件流增量推导出的会话运行态。
#[derive(Debug, Default)]
pub(super) struct RuntimeStatusProjection {
    // active profile 不落入持久化消息；新连接只能从运行态投影拿到当前值。
    active_profile: ActiveProfile,
    query_started_at: Option<DateTime<Utc>>,
    compact_started_at: Option<DateTime<Utc>>,
    query_pause_started_at: Option<DateTime<Utc>>,
    query_paused_ms: u64,
    query_state: protocol::SessionRuntimeState,
    pending_pauses: HashMap<String, RuntimePendingPause>,
    pending_plan_approval: Option<protocol::PlanSubmittedEvent>,
    active_tools: HashMap<String, RuntimeToolActivity>,
    subagents: HashMap<String, RuntimeSubagentActivity>,
}

/// 生成协议状态快照时由 session 层补充的外部上下文。
pub(super) struct RuntimeStatusSnapshotContext {
    pub loaded: bool,
    pub controller_id: Option<String>,
    pub connected_client_count: usize,
    pub skills: Vec<protocol::SessionRuntimeSkill>,
    pub mcp_servers: Vec<protocol::SessionRuntimeMcpServer>,
    pub now: DateTime<Utc>,
}

impl RuntimeStatusProjection {
    pub(super) fn with_active_profile(active_profile: ActiveProfile) -> Self {
        Self {
            active_profile,
            ..Self::default()
        }
    }

    pub(super) fn record_event(&mut self, event: &RuntimeEvent, now: DateTime<Utc>) {
        match runtime_replay_kind(event) {
            "active_profile_changed" => {
                if let Some(profile) = active_profile_payload(event) {
                    self.active_profile = profile;
                }
            }
            // 和 TUI 标签语义保持一致：run/turn 刚开始先显示 Thinking，直到可见输出或工具开始。
            "run_started" => self.start_query(now),
            "run_finished" | "session_changed" => self.clear_active_run(),
            "turn_started" => self.mark_query_thinking(),
            "text_delta" => self.mark_query_working(),
            "thinking_delta" => self.mark_query_thinking(),
            "tool_use" => self.record_tool_use(event, now, None, None),
            "tool_result" => {
                if let Some(tool_use_id) = event
                    .payload
                    .get("tool_use_id")
                    .and_then(serde_json::Value::as_str)
                {
                    self.finish_tool(tool_use_id);
                    self.finish_pause(tool_use_id, now);
                }
                self.mark_query_working();
            }
            "tool_pause_requested" => self.record_tool_pause(event, now),
            "plan_submitted" => {
                self.pending_plan_approval = plan_submitted_payload(event);
            }
            "plan_approval_resolved" => {
                if self.pending_plan_matches(event) {
                    self.pending_plan_approval = None;
                }
            }
            "compact_summary_started" => {
                self.compact_started_at = Some(now);
            }
            "compact_summary_finished" | "compact_summary_failed" => {
                self.compact_started_at = None;
                self.mark_query_working();
            }
            "subagent_started" => self.record_subagent_started(event, now),
            "subagent_tool_use" => self.record_subagent_tool_use(event, now),
            "subagent_tool_result" => self.record_subagent_tool_result(event, now),
            "subagent_finished" => self.record_subagent_finished(event, now),
            _ => {}
        }
    }

    pub(super) fn to_protocol(
        &self,
        session_id: &str,
        context: RuntimeStatusSnapshotContext,
    ) -> protocol::SessionRuntimeStatus {
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

        let mut subagents = self
            .subagents
            .values()
            .map(|subagent| subagent.to_protocol(context.now))
            .collect::<Vec<_>>();
        subagents.sort_by(|left, right| left.session_id.cmp(&right.session_id));

        protocol::SessionRuntimeStatus {
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
            skills: context.skills,
            mcp_servers: context.mcp_servers,
            subagents,
        }
    }

    fn start_query(&mut self, now: DateTime<Utc>) {
        self.query_started_at = Some(now);
        self.compact_started_at = None;
        self.query_pause_started_at = None;
        self.query_paused_ms = 0;
        self.query_state = protocol::SessionRuntimeState::Thinking;
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
        self.query_state = protocol::SessionRuntimeState::Idle;
        self.pending_pauses.clear();
        self.pending_plan_approval = None;
        self.active_tools.clear();
        self.subagents.clear();
    }

    fn mark_query_working(&mut self) {
        if self.query_started_at.is_some() {
            self.query_state = protocol::SessionRuntimeState::Working;
        }
    }

    fn mark_query_thinking(&mut self) {
        if self.query_started_at.is_some() {
            self.query_state = protocol::SessionRuntimeState::Thinking;
        }
    }

    fn record_tool_use(
        &mut self,
        event: &RuntimeEvent,
        now: DateTime<Utc>,
        source_session_id: Option<String>,
        source_agent_label: Option<String>,
    ) {
        let Some(tool_use) = tool_use_payload(event) else {
            return;
        };
        let Some(tool_use_id) = tool_use.get("id").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(tool_name) = tool_use.get("name").and_then(serde_json::Value::as_str) else {
            return;
        };
        self.record_tool(
            tool_use_id,
            tool_name,
            now,
            source_session_id,
            source_agent_label,
            tool_use.get("input"),
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
        input: Option<&serde_json::Value>,
    ) -> RuntimeToolActivity {
        let tool = RuntimeToolActivity {
            tool_use_id: tool_use_id.to_string(),
            tool_name: tool_name.to_string(),
            started_at: now,
            source_session_id,
            source_agent_label,
        };
        let _ = input;
        self.active_tools
            .insert(tool_use_id.to_string(), tool.clone());
        tool
    }

    fn finish_tool(&mut self, tool_use_id: &str) {
        self.active_tools.remove(tool_use_id);
    }

    fn record_tool_pause(&mut self, event: &RuntimeEvent, now: DateTime<Utc>) {
        let Some(tool_use_id) = event
            .payload
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let Some(tool_name) = event
            .payload
            .get("tool_name")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let Some(kind) = tool_pause_kind(event) else {
            return;
        };
        if self.query_started_at.is_some()
            && self.pending_pauses.is_empty()
            && self.query_pause_started_at.is_none()
        {
            self.query_pause_started_at = Some(now);
        }
        self.pending_pauses.insert(
            tool_use_id.to_string(),
            RuntimePendingPause {
                tool_use_id: tool_use_id.to_string(),
                tool_name: tool_name.to_string(),
                kind,
                source_session_id: event
                    .payload
                    .get("source_session_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                source_agent_label: event
                    .payload
                    .get("source_agent_label")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
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

    fn record_subagent_started(&mut self, event: &RuntimeEvent, now: DateTime<Utc>) {
        let Some(session_id) = event
            .payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let Some(agent_label) = event
            .payload
            .get("agent_label")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        self.subagents.insert(
            session_id.to_string(),
            RuntimeSubagentActivity {
                session_id: session_id.to_string(),
                agent_label: agent_label.to_string(),
                status: protocol::SubagentStatus::Running,
                started_at: now,
                finished_at: None,
                active_tool: None,
            },
        );
        self.mark_query_working();
    }

    fn record_subagent_tool_use(&mut self, event: &RuntimeEvent, now: DateTime<Utc>) {
        let Some(session_id) = event
            .payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let agent_label = self
            .subagents
            .get(session_id)
            .map(|subagent| subagent.agent_label.clone());
        let tool = self.record_tool_use_for_subagent(event, now, session_id, agent_label);
        if let Some(tool) = tool
            && let Some(subagent) = self.subagents.get_mut(session_id)
        {
            subagent.active_tool = Some(tool);
        }
        self.mark_query_working();
    }

    fn record_tool_use_for_subagent(
        &mut self,
        event: &RuntimeEvent,
        now: DateTime<Utc>,
        session_id: &str,
        agent_label: Option<String>,
    ) -> Option<RuntimeToolActivity> {
        let tool_use = event.payload.get("tool_use")?;
        let tool_use_id = tool_use.get("id").and_then(serde_json::Value::as_str)?;
        let tool_name = tool_use.get("name").and_then(serde_json::Value::as_str)?;
        Some(self.record_tool(
            tool_use_id,
            tool_name,
            now,
            Some(session_id.to_string()),
            agent_label,
            tool_use.get("input"),
        ))
    }

    fn record_subagent_tool_result(&mut self, event: &RuntimeEvent, now: DateTime<Utc>) {
        let Some(session_id) = event
            .payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let Some(tool_use_id) = event
            .payload
            .get("tool_result")
            .and_then(|tool_result| tool_result.get("tool_use_id"))
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        self.finish_tool(tool_use_id);
        self.finish_pause(tool_use_id, now);
        self.finish_pause(&format!("{session_id}:{tool_use_id}"), now);
        if let Some(subagent) = self.subagents.get_mut(session_id)
            && subagent
                .active_tool
                .as_ref()
                .is_some_and(|tool| tool.tool_use_id == tool_use_id)
        {
            subagent.active_tool = None;
        }
        self.mark_query_working();
    }

    fn record_subagent_finished(&mut self, event: &RuntimeEvent, now: DateTime<Utc>) {
        let Some(session_id) = event
            .payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        if let Some(subagent) = self.subagents.get_mut(session_id) {
            subagent.status = event
                .payload
                .get("status")
                .and_then(serde_json::Value::as_str)
                .and_then(subagent_status)
                .unwrap_or(protocol::SubagentStatus::Completed);
            subagent.finished_at = Some(now);
            subagent.active_tool = None;
        }
        self.mark_query_working();
    }

    fn state(&self) -> protocol::SessionRuntimeState {
        if self.compact_started_at.is_some() {
            protocol::SessionRuntimeState::Compacting
        } else if !self.pending_pauses.is_empty() {
            protocol::SessionRuntimeState::Waiting
        } else if self.query_started_at.is_some() {
            self.query_state
        } else {
            protocol::SessionRuntimeState::Idle
        }
    }

    fn activity(&self, now: DateTime<Utc>) -> Option<protocol::SessionRuntimeActivity> {
        if let Some(started_at) = self.compact_started_at {
            Some(protocol::SessionRuntimeActivity {
                kind: protocol::SessionRuntimeActivityKind::Compact,
                started_at,
                elapsed_ms: elapsed_ms(started_at, now),
            })
        } else {
            self.query_started_at
                .map(|started_at| protocol::SessionRuntimeActivity {
                    kind: protocol::SessionRuntimeActivityKind::Query,
                    started_at,
                    elapsed_ms: self.query_elapsed_ms(started_at, now),
                })
        }
    }

    pub(super) fn active_profile(&self) -> ActiveProfile {
        self.active_profile
    }

    fn pending_plan_matches(&self, event: &RuntimeEvent) -> bool {
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

fn elapsed_ms(started_at: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    now.signed_duration_since(started_at)
        .num_milliseconds()
        .max(0) as u64
}

fn tool_use_payload(event: &RuntimeEvent) -> Option<&serde_json::Value> {
    if runtime_replay_kind(event) == "tool_use" {
        Some(&event.payload)
    } else {
        event.payload.get("tool_use")
    }
}

fn tool_pause_kind(event: &RuntimeEvent) -> Option<protocol::ToolPauseEventKind> {
    match event
        .payload
        .get("kind")
        .and_then(|kind| kind.get("type"))
        .and_then(serde_json::Value::as_str)?
    {
        "permission" => Some(protocol::ToolPauseEventKind::Permission),
        "user_input" => Some(protocol::ToolPauseEventKind::UserInput),
        _ => None,
    }
}

fn active_profile_payload(event: &RuntimeEvent) -> Option<ActiveProfile> {
    match event
        .payload
        .get("profile")
        .and_then(serde_json::Value::as_str)?
    {
        "main" => Some(ActiveProfile::Main),
        "auto" => Some(ActiveProfile::Auto),
        "plan" => Some(ActiveProfile::Plan),
        _ => None,
    }
}

pub(super) fn plan_submitted_payload(event: &RuntimeEvent) -> Option<protocol::PlanSubmittedEvent> {
    if let Some(protocol::KeyRuntimeEvent::PlanSubmitted(plan)) = &event.event {
        return Some(plan.clone());
    }

    let plan_id = event
        .payload
        .get("plan_id")
        .or_else(|| event.payload.get("id"))
        .and_then(serde_json::Value::as_str)?;
    Some(protocol::PlanSubmittedEvent {
        plan_id: plan_id.to_string(),
        title: event
            .payload
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        markdown: event
            .payload
            .get("markdown")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

pub(super) fn plan_approval_resolved_plan_id(event: &RuntimeEvent) -> Option<String> {
    if let Some(protocol::KeyRuntimeEvent::PlanApprovalResolved(resolved)) = &event.event {
        return Some(resolved.plan_id.clone());
    }
    event
        .payload
        .get("plan_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn subagent_status(status: &str) -> Option<protocol::SubagentStatus> {
    match status {
        "running" => Some(protocol::SubagentStatus::Running),
        "completed" => Some(protocol::SubagentStatus::Completed),
        "failed" => Some(protocol::SubagentStatus::Failed),
        "cancelled" => Some(protocol::SubagentStatus::Cancelled),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequenced(seq: u64, kind: &str) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: RuntimeEvent::new(kind, serde_json::json!({ "type": kind })),
        }
    }

    fn delta(seq: u64, kind: &str, text: &str) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: RuntimeEvent::new(
                kind,
                serde_json::json!({
                    "type": kind,
                    "delta": text,
                }),
            ),
        }
    }

    fn tool_pause_event(tool_use_id: &str) -> RuntimeEvent {
        RuntimeEvent::new(
            "tool_pause_requested",
            serde_json::json!({
                "type": "tool_pause_requested",
                "tool_use_id": tool_use_id,
                "tool_name": "bash",
                "kind": { "type": "permission", "preview": {} }
            }),
        )
    }

    fn tool_result_event(tool_use_id: &str) -> RuntimeEvent {
        RuntimeEvent::new(
            "tool_result",
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "is_error": false,
                "content": "done"
            }),
        )
    }

    fn status_snapshot(
        projection: &RuntimeStatusProjection,
        now: DateTime<Utc>,
    ) -> protocol::SessionRuntimeStatus {
        projection.to_protocol(
            "s1",
            RuntimeStatusSnapshotContext {
                loaded: true,
                controller_id: Some("client_1".to_string()),
                connected_client_count: 1,
                skills: Vec::new(),
                mcp_servers: Vec::new(),
                now,
            },
        )
    }

    #[test]
    fn runtime_status_projection_tracks_active_profile() {
        let mut projection = RuntimeStatusProjection::default();

        projection.record_event(
            &RuntimeEvent::new(
                "active_profile_changed",
                serde_json::json!({
                    "type": "active_profile_changed",
                    "profile": "plan"
                }),
            ),
            Utc::now(),
        );

        assert_eq!(projection.active_profile(), ActiveProfile::Plan);
        assert_eq!(
            status_snapshot(&projection, Utc::now()).active_profile,
            protocol::ActiveProfile::Plan
        );
    }

    #[test]
    fn runtime_status_projection_tracks_pending_plan_approval() {
        let mut projection = RuntimeStatusProjection::default();
        let now = Utc::now();

        projection.record_event(
            &RuntimeEvent::new(
                "plan_submitted",
                serde_json::json!({
                    "type": "plan_submitted",
                    "id": "plan_1",
                    "title": "Plan",
                    "markdown": "# Plan"
                }),
            ),
            now,
        );
        let status = status_snapshot(&projection, now);
        assert_eq!(
            status
                .pending_plan_approval
                .as_ref()
                .map(|plan| plan.plan_id.as_str()),
            Some("plan_1")
        );

        projection.record_event(
            &RuntimeEvent::new(
                "plan_approval_resolved",
                serde_json::json!({
                    "type": "plan_approval_resolved",
                    "plan_id": "plan_1",
                    "action": { "type": "continue_discussing" }
                }),
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
        assert_eq!(status.state, protocol::SessionRuntimeState::Thinking);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.kind),
            Some(protocol::SessionRuntimeActivityKind::Query)
        );
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(42)
        );

        projection.record_event(&sequenced(2, "turn_started").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            protocol::SessionRuntimeState::Thinking
        );

        projection.record_event(&delta(3, "thinking_delta", "hmm").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            protocol::SessionRuntimeState::Thinking
        );

        projection.record_event(&delta(4, "text_delta", "hello").event, started_at);
        assert_eq!(
            status_snapshot(&projection, started_at).state,
            protocol::SessionRuntimeState::Working
        );

        projection.record_event(
            &tool_pause_event("tool_1"),
            started_at + chrono::Duration::milliseconds(50),
        );
        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(70));
        assert_eq!(status.state, protocol::SessionRuntimeState::Waiting);
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
        assert_eq!(status.state, protocol::SessionRuntimeState::Working);
        assert!(status.pending_pauses.is_empty());
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(80)
        );

        projection.record_event(&sequenced(3, "run_finished").event, started_at);
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.state, protocol::SessionRuntimeState::Idle);
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
        assert_eq!(status.state, protocol::SessionRuntimeState::Waiting);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(10)
        );

        projection.record_event(
            &tool_result_event("tool_1"),
            started_at + chrono::Duration::milliseconds(60),
        );
        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(70));
        assert_eq!(status.state, protocol::SessionRuntimeState::Waiting);
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
        assert_eq!(status.state, protocol::SessionRuntimeState::Working);
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
            &RuntimeEvent::new(
                "compact_summary_started",
                serde_json::json!({
                    "type": "compact_summary_started",
                    "trigger": "manual",
                    "session_id": "s1",
                    "agent_label": null
                }),
            ),
            started_at,
        );

        let status = status_snapshot(&projection, started_at + chrono::Duration::milliseconds(7));
        assert_eq!(status.state, protocol::SessionRuntimeState::Compacting);
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.kind),
            Some(protocol::SessionRuntimeActivityKind::Compact)
        );
        assert_eq!(
            status.activity.as_ref().map(|activity| activity.elapsed_ms),
            Some(7)
        );

        projection.record_event(
            &RuntimeEvent::new(
                "compact_summary_finished",
                serde_json::json!({
                    "type": "compact_summary_finished",
                    "trigger": "manual",
                    "summary": "done",
                    "after_tokens": 1,
                    "session_id": "s1",
                    "agent_label": null
                }),
            ),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.state, protocol::SessionRuntimeState::Idle);
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
                skills: vec![protocol::SessionRuntimeSkill {
                    name: "writer".to_string(),
                    description: "Write carefully".to_string(),
                    source_kind: protocol::SkillSourceKind::Project,
                    directory: "/repo/.omini/skills/writer".to_string(),
                    status: protocol::SessionRuntimeCapabilityStatus::Available,
                    inject: true,
                    user_invocable: true,
                }],
                mcp_servers: vec![protocol::SessionRuntimeMcpServer {
                    name: "docs".to_string(),
                    status: protocol::SessionRuntimeMcpStatus::Ready,
                    last_error: None,
                    tools: vec![protocol::SessionRuntimeMcpTool {
                        name: "search".to_string(),
                        registered_name: "mcp__docs__search".to_string(),
                        description: "Search docs".to_string(),
                    }],
                }],
                now,
            },
        );

        assert_eq!(status.state, protocol::SessionRuntimeState::Idle);
        assert_eq!(status.skills.len(), 1);
        assert_eq!(
            status.skills[0].source_kind,
            protocol::SkillSourceKind::Project
        );
        assert_eq!(status.mcp_servers.len(), 1);
        assert_eq!(
            status.mcp_servers[0].status,
            protocol::SessionRuntimeMcpStatus::Ready
        );
        assert_eq!(
            status.mcp_servers[0].tools[0].registered_name,
            "mcp__docs__search"
        );
    }

    #[test]
    fn runtime_status_tracks_active_tools_and_subagents() {
        let mut projection = RuntimeStatusProjection::default();
        let started_at = Utc::now();

        projection.record_event(&sequenced(1, "run_started").event, started_at);
        projection.record_event(
            &RuntimeEvent::new(
                "tool_use",
                serde_json::json!({
                    "type": "tool_use",
                    "id": "tool_skill",
                    "name": "skill",
                    "input": { "name": "rust" }
                }),
            ),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.active_tools.len(), 1);
        assert_eq!(status.active_tools[0].tool_name, "skill");

        projection.record_event(
            &RuntimeEvent::new(
                "subagent_started",
                serde_json::json!({
                    "type": "subagent_started",
                    "session_id": "sub_1",
                    "parent_session_id": "s1",
                    "spawn_tool_use_id": "tool_subagent",
                    "agent_label": "explorer"
                }),
            ),
            started_at,
        );
        projection.record_event(
            &RuntimeEvent::new(
                "subagent_tool_use",
                serde_json::json!({
                    "type": "subagent_tool_use",
                    "session_id": "sub_1",
                    "tool_use": {
                        "id": "sub_tool_1",
                        "name": "read",
                        "input": {}
                    }
                }),
            ),
            started_at,
        );
        let status = status_snapshot(&projection, started_at);
        assert_eq!(status.subagents.len(), 1);
        assert_eq!(status.subagents[0].agent_label, "explorer");
        assert_eq!(
            status.subagents[0]
                .active_tool
                .as_ref()
                .map(|tool| tool.tool_name.as_str()),
            Some("read")
        );
    }
}
