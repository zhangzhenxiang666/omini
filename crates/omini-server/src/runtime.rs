//! daemon 内部的项目、会话、事件重放和控制权状态。
//!
//! `omini-server` 在这里把 HTTP/WS 层的会话语义适配到 `omini-core`：创建或恢复
//! runtime session、维护客户端 presence、裁剪重连 replay、落盘 core persistence event，
//! 并把当前运行态投影成 protocol DTO。

use chrono::{DateTime, Utc};
use omini_core::AgentCoreSession;
use omini_core::CoreError;
use omini_core::config::project::ProjectDir;
use omini_core::config::settings::OminiRoot;
use omini_core::config::settings::UserConfig;
use omini_core::persistence::RuntimePersistenceEvent;
use omini_core::types::display::HistoryItem;
use omini_core::types::events::{ActiveProfile, LoadedSession};
use omini_core::types::message::{ContentBlock, Message, Role};
use omini_domain::project::sanitize_project_path as sanitize;
use omini_protocol as protocol;
use omini_protocol::RuntimeEvent;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use crate::history;
use crate::store::{Database, Session};

/// 项目 attach 入口的错误分类，路由层会映射成协议错误。
pub(crate) enum ProjectAttachError {
    BadRequest(String),
    Config(String),
    Core(CoreError),
}

impl From<CoreError> for ProjectAttachError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

/// daemon 尚未认识某个项目时的查找错误。
pub(crate) enum ProjectLookupError {
    NotFound,
}

/// 会话查找或恢复过程中可能出现的错误。
pub(crate) enum SessionError {
    NotFound,
    Core(CoreError),
}

impl From<CoreError> for SessionError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

/// daemon 级项目注册表，负责把 project_id 路由到对应的 `SessionManager`。
pub(crate) struct GlobalDaemonManager {
    root: OminiRoot,
    config: UserConfig,
    db: Arc<Database>,
    // daemon 按项目隔离 SessionManager；HTTP 路由里的 project_id 必须先 attach 才能命中这里。
    projects: Mutex<HashMap<String, Arc<SessionManager>>>,
}

impl GlobalDaemonManager {
    pub(crate) fn new(root: OminiRoot, config: UserConfig, db: Arc<Database>) -> Self {
        Self {
            root,
            config,
            db,
            projects: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn attach_project(
        &self,
        project_id: &str,
        cwd: PathBuf,
    ) -> Result<protocol::ProjectAttachResponse, ProjectAttachError> {
        // project_id 由 cwd 派生，服务端重新计算一次，避免客户端把请求挂到错误项目上。
        let expected_project_id = sanitize(&cwd);
        if project_id != expected_project_id {
            return Err(ProjectAttachError::BadRequest(format!(
                "Project id '{project_id}' does not match cwd '{expected_project_id}'"
            )));
        }

        let project = self
            .root
            .init_project(&cwd, &self.config)
            .map_err(|err| ProjectAttachError::Config(err.to_string()))?;
        let project_state = project
            .load_state()
            .map_err(|err| ProjectAttachError::Config(err.to_string()))?;
        let mut settings = self
            .config
            .to_settings(
                project_state.default_provider.as_deref(),
                project_state.default_model.as_deref(),
                project_state.thinking_effort,
            )
            .map_err(|err| ProjectAttachError::Config(err.to_string()))?;
        settings.cwd = cwd.clone();

        let manager = {
            let mut projects = self.projects.lock().expect("projects lock poisoned");
            if let Some(manager) = projects.get(project_id) {
                // 同一项目重复 attach 复用已有 manager，避免拆出多套 session/cache 状态。
                Arc::clone(manager)
            } else {
                let manager =
                    Arc::new(SessionManager::new(settings, project, Arc::clone(&self.db)));
                projects.insert(project_id.to_string(), Arc::clone(&manager));
                manager
            }
        };

        manager
            .attach_response(project_id)
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn project(
        &self,
        project_id: &str,
    ) -> Result<Arc<SessionManager>, ProjectLookupError> {
        self.projects
            .lock()
            .expect("projects lock poisoned")
            .get(project_id)
            .cloned()
            .ok_or(ProjectLookupError::NotFound)
    }
}

/// 单个项目下的会话管理器。
///
/// 它只缓存当前有客户端使用的 runtime session；持久化会话列表和历史仍来自数据库。
pub(crate) struct SessionManager {
    settings: omini_core::types::config::Settings,
    project: ProjectDir,
    db: Arc<Database>,
    // 这里只缓存正在被客户端使用的 runtime；空闲后会关闭并从数据库按需恢复。
    sessions: Mutex<HashMap<String, Arc<RuntimeSession>>>,
}

impl SessionManager {
    pub(crate) fn new(
        settings: omini_core::types::config::Settings,
        project: ProjectDir,
        db: Arc<Database>,
    ) -> Self {
        Self {
            settings,
            project,
            db,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn attach_response(
        &self,
        project_id: &str,
    ) -> Result<protocol::ProjectAttachResponse, CoreError> {
        let sessions = self.list_sessions().await?.sessions;
        let context_window = self
            .settings
            .current_model_config()
            .map(|model| model.limit);
        let mcp_server_count = self
            .settings
            .mcp_servers
            .values()
            .filter(|server| server.enabled)
            .count();
        let has_project_instructions = self
            .settings
            .cwd
            .join("AGENTS.md")
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0);
        let show_thinking_blocks = self
            .project
            .load_state()
            .map(|state| state.show_thinking_blocks)
            .unwrap_or(true);
        let agents = omini_core::subagents::list_agent_records(&self.settings.cwd)
            .into_iter()
            .map(|agent| protocol::AgentSummary {
                name: agent.name,
                description: agent.description,
            })
            .collect();
        let skills = omini_core::skills::load_skill_registry(&self.settings.cwd)
            .skills()
            .filter(|skill| skill.user_invocable)
            .map(|skill| protocol::SkillSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect();

        Ok(protocol::ProjectAttachResponse {
            project_id: project_id.to_string(),
            cwd: self.settings.cwd.display().to_string(),
            sessions,
            active_provider: self.settings.active_provider.clone(),
            model: self.settings.model.clone(),
            thinking_effort: self
                .settings
                .thinking_effort
                .map(thinking_effort_to_protocol),
            context_window,
            mcp_server_count,
            has_project_instructions,
            show_thinking_blocks,
            agents,
            skills,
        })
    }

    pub(crate) async fn list_sessions(&self) -> Result<protocol::SessionsResponse, CoreError> {
        let project_path = sanitize(&self.settings.cwd);
        let sessions = self
            .db
            .list_sessions(&project_path)
            .await
            .map_err(|error| CoreError::new(format!("Failed to list sessions: {error}")))?
            .into_iter()
            .map(session_summary_from_store)
            .collect();
        Ok(protocol::SessionsResponse { sessions })
    }

    pub(crate) async fn list_session_statuses(
        &self,
        filter: Option<&[protocol::SessionRuntimeState]>,
    ) -> protocol::SessionStatusesResponse {
        let mut sessions = {
            let sessions = self.sessions.lock().expect("sessions lock poisoned");
            sessions.values().cloned().collect::<Vec<_>>()
        };
        sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));

        let mut statuses = Vec::new();
        for session in sessions {
            let status = session.runtime_status().await;
            let include = filter
                .map(|states| states.contains(&status.state))
                .unwrap_or(true);
            if include {
                statuses.push(status);
            }
        }

        protocol::SessionStatusesResponse { statuses }
    }

    pub(crate) async fn session_status(
        &self,
        session_id: &str,
    ) -> Option<protocol::SessionRuntimeStatusResponse> {
        let session = {
            self.sessions
                .lock()
                .expect("sessions lock poisoned")
                .get(session_id)
                .cloned()
        }?;
        Some(protocol::SessionRuntimeStatusResponse {
            status: session.runtime_status().await,
        })
    }

    pub(crate) async fn create_session(
        &self,
    ) -> Result<protocol::CreateSessionResponse, CoreError> {
        let session_id = uuid::Uuid::new_v4().to_string();
        self.project.create_session(&session_id).map_err(|error| {
            CoreError::new(format!("Failed to create session directory: {error}"))
        })?;
        let now = chrono::Utc::now();
        let session = Session {
            id: session_id.clone(),
            project_path: sanitize(&self.settings.cwd),
            parent_session_id: None,
            spawn_tool_use_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: self.settings.active_provider.clone(),
            model: self.settings.model.clone(),
            thinking_effort: self
                .settings
                .thinking_effort
                .map(|effort| effort.to_string()),
            title: None,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            created_at: now,
            updated_at: now,
        };
        self.db
            .create_session(&session)
            .await
            .map_err(|error| CoreError::new(format!("Failed to persist session: {error}")))?;
        let runtime = Arc::new(RuntimeSession::spawn(
            self.settings.clone(),
            self.project.clone(),
            session_id.clone(),
            Arc::clone(&self.db),
        ));
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .insert(session_id.clone(), runtime);
        Ok(protocol::CreateSessionResponse {
            session_id: Some(session_id),
        })
    }

    pub(crate) async fn session(
        &self,
        session_id: &str,
    ) -> Result<Arc<RuntimeSession>, SessionError> {
        let cached = {
            self.sessions
                .lock()
                .expect("sessions lock poisoned")
                .get(session_id)
                .cloned()
        };
        if let Some(session) = cached {
            return Ok(session);
        }

        let project_path = sanitize(&self.settings.cwd);
        let Some(session_record) = self
            .db
            .get_session(session_id)
            .await
            .map_err(|error| CoreError::new(format!("Failed to load session: {error}")))?
        else {
            return Err(SessionError::NotFound);
        };
        if session_record.project_path != project_path || session_record.parent_session_id.is_some()
        {
            return Err(SessionError::NotFound);
        }

        // 数据库查询和 runtime 创建之间可能有并发请求，拿到锁后再检查一次缓存。
        let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
        if let Some(session) = sessions.get(session_id) {
            return Ok(Arc::clone(session));
        }
        let session = Arc::new(RuntimeSession::spawn(
            self.settings.clone(),
            self.project.clone(),
            session_id.to_string(),
            Arc::clone(&self.db),
        ));
        sessions.insert(session_id.to_string(), Arc::clone(&session));
        Ok(session)
    }

    pub(crate) async fn close_session_if_idle(
        &self,
        session_id: &str,
        session: &Arc<RuntimeSession>,
    ) {
        let should_close = {
            let presence = session.presence.lock().expect("presence lock poisoned");
            if !presence.clients.is_empty() {
                return;
            }
            let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
            let Some(current) = sessions.get(session_id) else {
                return;
            };
            // 只关闭当前缓存里的同一个 Arc，避免旧连接清理时误关掉新建 runtime。
            if Arc::ptr_eq(current, session) {
                sessions.remove(session_id);
                true
            } else {
                false
            }
        };

        if should_close && let Err(error) = session.shutdown().await {
            eprintln!("runtime session shutdown failed: {error}");
        }
    }
}

/// 单个 runtime session 中的客户端在线状态和 controller 归属。
#[derive(Debug, Default)]
struct ClientPresence {
    // 同一 client_id 可能打开多个 WebSocket，计数归零才算真正离线。
    clients: HashMap<String, usize>,
    // controller 永远只能是在线客户端；释放/断开时会自动转给其它在线客户端。
    controller_id: Option<String>,
}

impl ClientPresence {
    fn register(&mut self, client_id: String) -> (Option<String>, bool) {
        let before = self.controller_id.clone();
        *self.clients.entry(client_id.clone()).or_insert(0) += 1;
        if self.controller_id.is_none() {
            self.controller_id = Some(client_id);
        }
        let after = self.controller_id.clone();
        (after.clone(), before != after)
    }

    fn unregister(&mut self, client_id: &str) -> (Option<String>, bool) {
        let before = self.controller_id.clone();
        if let Some(count) = self.clients.get_mut(client_id) {
            if *count > 1 {
                *count -= 1;
                return (self.controller_id.clone(), false);
            }
            self.clients.remove(client_id);
        }

        if before.as_deref() == Some(client_id) {
            self.controller_id = self.random_client_id(None);
        }
        let after = self.controller_id.clone();
        (after.clone(), before != after)
    }

    fn claim(&mut self, client_id: String) -> Option<(Option<String>, bool)> {
        if !self.clients.contains_key(&client_id) {
            return None;
        }
        let before = self.controller_id.clone();
        // claim 只在当前没有 controller 时生效，避免观察者无意覆盖已有控制者。
        if self.controller_id.is_none() {
            self.controller_id = Some(client_id);
        }
        let after = self.controller_id.clone();
        Some((after.clone(), before != after))
    }

    fn takeover(&mut self, client_id: String) -> Option<(Option<String>, bool)> {
        if !self.clients.contains_key(&client_id) {
            return None;
        }
        let before = self.controller_id.clone();
        // takeover 是显式抢占入口，调用方必须已经确认这是用户意图或安全的自动接管。
        self.controller_id = Some(client_id);
        let after = self.controller_id.clone();
        Some((after.clone(), before != after))
    }

    fn release(&mut self, client_id: &str) -> (Option<String>, bool) {
        let before = self.controller_id.clone();
        if before.as_deref() == Some(client_id) {
            // 释放 controller 后仍保留“有连接就有控制者”的不变量。
            self.controller_id = self.random_client_id(Some(client_id));
        }
        let after = self.controller_id.clone();
        (after.clone(), before != after)
    }

    fn random_client_id(&self, exclude: Option<&str>) -> Option<String> {
        let candidates = self
            .clients
            .keys()
            .filter(|candidate| exclude != Some(candidate.as_str()))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        Some(candidates[random_index(candidates.len())].to_string())
    }
}

fn random_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let random = uuid::Uuid::new_v4();
    let mut value = 0usize;
    for byte in random.as_bytes().iter().take(std::mem::size_of::<usize>()) {
        value = (value << 8) | usize::from(*byte);
    }
    value % len
}

/// 带本地单调序号的 runtime 事件，用于 WebSocket replay 和实时订阅去重。
#[derive(Clone)]
pub(crate) struct SequencedRuntimeEvent {
    // seq 只在单个 RuntimeSession 内单调递增，用来让 WebSocket replay 和订阅流去重。
    pub(crate) seq: u64,
    pub(crate) event: RuntimeEvent,
}

/// 保存重连时必须补发、但尚未被持久化 snapshot 覆盖的运行中事件尾部。
#[derive(Default)]
struct RuntimeReplayBuffer {
    // run_started 之前的用户注入事件先暂存，确保重连客户端能看到刚提交的输入。
    pending_prefix: Vec<SequencedRuntimeEvent>,
    // run_started 是 replay 的锚点；run 结束或 session_changed 后会清空。
    run_started: Option<SequencedRuntimeEvent>,
    // 当前 turn 尚未被持久化 snapshot 覆盖的尾部增量。
    current_tail: Vec<SequencedRuntimeEvent>,
    // compact 不一定发生在 query run 内；单独保留它的流式尾部供新连接补齐。
    compact_started: Option<SequencedRuntimeEvent>,
    compact_tail: Vec<SequencedRuntimeEvent>,
    // 计划确认发生在 run_finished 后，不能依赖 run tail；保留到任一客户端完成确认。
    pending_plan_approval: Option<SequencedRuntimeEvent>,
}

impl RuntimeReplayBuffer {
    fn record(&mut self, event: SequencedRuntimeEvent) {
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
            "session_changed" => {
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

    fn replay(&self) -> Vec<SequencedRuntimeEvent> {
        let mut replay = Vec::with_capacity(
            self.pending_prefix.len()
                + usize::from(self.run_started.is_some())
                + self.current_tail.len()
                + usize::from(self.compact_started.is_some())
                + self.compact_tail.len()
                + usize::from(self.pending_plan_approval.is_some()),
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
        replay
    }

    fn record_persistence(&mut self, owner_session_id: &str, event: &RuntimePersistenceEvent) {
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

    fn record_snapshot(&mut self, snapshot: &LoadedSession) {
        // 新连接发 snapshot 前再做一次裁剪，覆盖持久化事件和 snapshot 生成之间的竞态。
        self.drop_user_injections_in_snapshot(snapshot);
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

fn runtime_replay_kind(event: &RuntimeEvent) -> &str {
    event
        .payload
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(event.kind.as_str())
}

/// 判断待 replay 的用户注入事件是否已经出现在持久化 snapshot 中。
fn user_injection_is_in_snapshot(event: &SequencedRuntimeEvent, snapshot: &LoadedSession) -> bool {
    if runtime_replay_kind(&event.event) != "user_message_injected" {
        return false;
    }
    let Some(item) = event.event.payload.get("item") else {
        return false;
    };
    snapshot
        .messages
        .iter()
        .filter_map(|message| serde_json::to_value(message).ok())
        .any(|message| message == *item)
}

/// 把当前 assistant 流式尾部重组为完整内容块，供 snapshot 去重比较。
fn assistant_tail_blocks(events: &[SequencedRuntimeEvent]) -> Vec<ContentBlock> {
    // 增量事件需要还原成完整 ContentBlock，才能和 snapshot 中的 assistant message 比较。
    let mut blocks = Vec::new();
    for event in events {
        match runtime_replay_kind(&event.event) {
            "thinking_delta" => push_delta_block(&mut blocks, event, true),
            "text_delta" => push_delta_block(&mut blocks, event, false),
            "tool_use" => {
                if let Ok(block @ ContentBlock::ToolUse(_)) =
                    serde_json::from_value(event.event.payload.clone())
                {
                    blocks.push(block);
                }
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
        .filter(|event| runtime_replay_kind(&event.event) == "tool_result")
        .filter_map(|event| serde_json::from_value(event.event.payload.clone()).ok())
        .collect()
}

/// 将连续文本或 thinking delta 合并成可比较的 `ContentBlock`。
fn push_delta_block(blocks: &mut Vec<ContentBlock>, event: &SequencedRuntimeEvent, thinking: bool) {
    let Some(delta) = event
        .event
        .payload
        .get("delta")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };

    match (thinking, blocks.last_mut()) {
        (true, Some(ContentBlock::Thinking(block))) => block.thinking.push_str(delta),
        (false, Some(ContentBlock::Text(block))) => block.text.push_str(delta),
        (true, _) => blocks.push(ContentBlock::from_thinking(delta.to_string())),
        (false, _) => blocks.push(ContentBlock::from_text(delta.to_string())),
    }
}

/// 当前仍在执行的工具调用投影。
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
struct RuntimeStatusProjection {
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
struct RuntimeStatusSnapshotContext {
    loaded: bool,
    controller_id: Option<String>,
    connected_client_count: usize,
    skills: Vec<protocol::SessionRuntimeSkill>,
    mcp_servers: Vec<protocol::SessionRuntimeMcpServer>,
    now: DateTime<Utc>,
}

impl RuntimeStatusProjection {
    fn record_event(&mut self, event: &RuntimeEvent, now: DateTime<Utc>) {
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

    fn to_protocol(
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

    fn active_profile(&self) -> ActiveProfile {
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

fn plan_submitted_payload(event: &RuntimeEvent) -> Option<protocol::PlanSubmittedEvent> {
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

fn plan_approval_resolved_plan_id(event: &RuntimeEvent) -> Option<String> {
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

type RuntimeLoadWaiter = oneshot::Sender<Result<(), String>>;

/// core snapshot hydrate 的加载状态。
#[derive(Default)]
enum RuntimeLoadState {
    #[default]
    NotLoaded,
    Loading {
        waiters: Vec<RuntimeLoadWaiter>,
    },
    Loaded,
}

/// `RuntimeLoadGate` 判断调用方该直接返回、负责加载，还是等待已有加载。
enum RuntimeLoadAction {
    AlreadyLoaded,
    Load,
    Wait(oneshot::Receiver<Result<(), String>>),
}

/// 确保同一个 session 的 core snapshot 只被一个任务加载，其他请求共享结果。
#[derive(Default)]
struct RuntimeLoadGate {
    state: Mutex<RuntimeLoadState>,
}

impl RuntimeLoadGate {
    fn begin_load(&self) -> RuntimeLoadAction {
        let mut loaded = self.state.lock().expect("loaded state lock poisoned");
        match &mut *loaded {
            RuntimeLoadState::Loaded => RuntimeLoadAction::AlreadyLoaded,
            RuntimeLoadState::NotLoaded => {
                *loaded = RuntimeLoadState::Loading {
                    waiters: Vec::new(),
                };
                RuntimeLoadAction::Load
            }
            RuntimeLoadState::Loading { waiters } => {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                RuntimeLoadAction::Wait(rx)
            }
        }
    }

    fn finish_load(&self, result: &Result<(), CoreError>) {
        let mut loaded = self.state.lock().expect("loaded state lock poisoned");
        let waiters = match &mut *loaded {
            RuntimeLoadState::Loading { waiters } => {
                let waiters = std::mem::take(waiters);
                *loaded = if result.is_ok() {
                    RuntimeLoadState::Loaded
                } else {
                    RuntimeLoadState::NotLoaded
                };
                waiters
            }
            RuntimeLoadState::NotLoaded | RuntimeLoadState::Loaded => Vec::new(),
        };
        drop(loaded);

        let error = result
            .as_ref()
            .err()
            .map(|error| error.message().to_string());
        for waiter in waiters {
            let _ = waiter.send(match &error {
                Some(message) => Err(message.clone()),
                None => Ok(()),
            });
        }
    }

    fn is_loaded(&self) -> bool {
        matches!(
            *self.state.lock().expect("loaded state lock poisoned"),
            RuntimeLoadState::Loaded
        )
    }
}

/// server 对单个 core 会话的适配层。
///
/// 它连接 core runtime、数据库、WebSocket fanout、controller presence、runtime status
/// projection 和 replay buffer。HTTP 路由拿到的 `RuntimeSession` 不直接操作 core 的内部
/// loop，而是通过这个类型做 daemon 级的持久化、重连补发和多客户端控制权协调。
pub(crate) struct RuntimeSession {
    // 单个 daemon session 对应的 core facade；HTTP/controller 校验后的用户动作从这里进入 core。
    pub(crate) core: AgentCoreSession,
    // daemon 会话 ID，同时也是数据库、项目 session 目录和 WebSocket 路由使用的稳定 ID。
    session_id: String,
    // 当前项目的目录句柄，用于加载 session snapshot、subagent 历史和 block 文件。
    project: ProjectDir,
    // 创建 runtime 时的项目配置快照；server 用它补充 snapshot/status 中的只读信息。
    settings: omini_core::types::config::Settings,
    // session 元数据、消息、usage 和 core persistence event 的 SQLite 存储。
    db: Arc<Database>,
    // core runtime 事件经过本地 seq 编号后的广播流，WebSocket 订阅和 replay 去重都用它。
    runtime_event_tx: broadcast::Sender<SequencedRuntimeEvent>,
    // server 本地产生的协议事件广播流，例如 session title 变更，不经过 core runtime。
    server_event_tx: broadcast::Sender<RuntimeEvent>,
    // 当前连接的 client 集合和 controller 归属；HTTP mutation 会用它做控制权检查。
    presence: Mutex<ClientPresence>,
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
    fn spawn(
        settings: omini_core::types::config::Settings,
        project: ProjectDir,
        session_id: String,
        db: Arc<Database>,
    ) -> Self {
        let (controller_tx, _) = broadcast::channel(32);
        let (runtime_event_tx, _) = broadcast::channel(512);
        let (server_event_tx, _) = broadcast::channel(128);
        let core = AgentCoreSession::spawn(settings.clone(), project.clone());
        let mut persistence_rx = core.subscribe_persistence();
        let mut tool_pause_rx = core.subscribe();
        let mut runtime_event_rx = core.subscribe();
        let replay_buffer = Arc::new(Mutex::new(RuntimeReplayBuffer::default()));
        let status_projection = Arc::new(Mutex::new(RuntimeStatusProjection::default()));
        let persistence_db = Arc::clone(&db);
        let persisted_replay_buffer = Arc::clone(&replay_buffer);
        let replay_session_id = session_id.clone();
        // core 发出的持久化事件先落 SQLite，成功后再裁剪 replay，避免重连时漏掉未落盘内容。
        let persistence_handle = tokio::spawn(async move {
            loop {
                match persistence_rx.recv().await {
                    Ok(event) => {
                        let persisted_event = event.clone();
                        if let Err(error) = persistence_db.apply_persistence_event(event).await {
                            eprintln!("runtime persistence event failed: {error}");
                        } else {
                            persisted_replay_buffer
                                .lock()
                                .expect("replay buffer lock poisoned")
                                .record_persistence(&replay_session_id, &persisted_event);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        eprintln!("runtime persistence event stream lagged; skipped {skipped}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let runtime_event_fanout_tx = runtime_event_tx.clone();
        let runtime_replay_buffer = Arc::clone(&replay_buffer);
        let runtime_status_projection = Arc::clone(&status_projection);
        // runtime 事件加上本地 seq 后再广播，WebSocket 层用 seq 处理 replay/订阅交叠。
        let runtime_event_handle = tokio::spawn(async move {
            let mut next_seq = 1u64;
            loop {
                match runtime_event_rx.recv().await {
                    Ok(event) => {
                        let sequenced = SequencedRuntimeEvent {
                            seq: next_seq,
                            event,
                        };
                        next_seq = next_seq.saturating_add(1);
                        runtime_replay_buffer
                            .lock()
                            .expect("replay buffer lock poisoned")
                            .record(sequenced.clone());
                        runtime_status_projection
                            .lock()
                            .expect("status projection lock poisoned")
                            .record_event(&sequenced.event, Utc::now());
                        let _ = runtime_event_fanout_tx.send(sequenced);
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        eprintln!("runtime event stream lagged; skipped {skipped}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let pending_tool_pauses = Arc::new(Mutex::new(HashSet::new()));
        let pending_tool_pause_events = Arc::clone(&pending_tool_pauses);
        // 工具暂停状态跟随 runtime 事件维护，HTTP resolve 用它做幂等和重复点击保护。
        let tool_pause_handle = tokio::spawn(async move {
            loop {
                match tool_pause_rx.recv().await {
                    Ok(event) => {
                        apply_tool_pause_update(&pending_tool_pause_events, &event);
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        eprintln!("runtime tool pause event stream lagged; skipped {skipped}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        Self {
            core,
            session_id,
            project,
            settings,
            db,
            runtime_event_tx,
            server_event_tx,
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

    pub(crate) fn subscribe_server_events(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.server_event_tx.subscribe()
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
                    now: Utc::now(),
                },
            )
    }

    pub(crate) async fn rename_session(&self, title: String) -> Result<(), CoreError> {
        self.db
            .update_session_title(&self.session_id, &title)
            .await
            .map_err(|error| CoreError::new(format!("Failed to rename session: {error}")))?;
        self.broadcast_session_title_changed(title);
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
                CoreError::new(format!("Failed to set initial session title: {error}"))
            })?;
        if updated {
            self.broadcast_session_title_changed(title);
        }
        Ok(())
    }

    fn broadcast_session_title_changed(&self, title: String) {
        let payload = serde_json::json!({
            "type": "session_title_changed",
            "title": title,
        });
        let _ = self
            .server_event_tx
            .send(RuntimeEvent::new("session_title_changed", payload));
    }

    pub(crate) async fn ensure_loaded(&self) -> Result<(), CoreError> {
        match self.loaded.begin_load() {
            RuntimeLoadAction::AlreadyLoaded => Ok(()),
            RuntimeLoadAction::Wait(waiter) => match waiter.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(message)) => Err(CoreError::new(message)),
                Err(_) => Err(CoreError::new("Runtime session loading was interrupted")),
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
            .map_err(|error| CoreError::new(format!("Failed to load session: {error}")))?
            .ok_or_else(|| CoreError::new("Session does not exist"))?;
        let session_dir = self.project.session(&self.session_id);
        let blocks_dir = session_dir.path().join("blocks");
        let messages = history::load_messages(&self.db, &self.session_id, &blocks_dir).await;
        let subagents =
            history::load_subagents_for_session(&self.db, &self.session_id, &self.project).await;
        Ok(omini_core::types::events::LoadedSession {
            session_id: session.id,
            provider: session.provider,
            model: session.model,
            thinking_effort: session
                .thinking_effort
                .as_deref()
                .and_then(|effort| effort.parse().ok()),
            title: session.title,
            messages,
            subagents,
            usage: omini_core::types::events::SessionUsageSnapshot {
                current_context_tokens: session.current_context_tokens,
                total_tokens: session.total_tokens,
                total_cached_tokens: session.total_cached_tokens,
                context_window: None,
            },
        })
    }

    fn context_window_for_snapshot(
        &self,
        snapshot: &omini_core::types::events::LoadedSession,
    ) -> Option<u32> {
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
}

/// 工具暂停 resolve 请求开始前的幂等检查结果。
pub(crate) enum ToolPauseResolutionStart {
    Started,
    AlreadyResolved,
    ClientNotConnected,
}

/// runtime 事件对 pending tool pause 集合的影响。
#[derive(Debug, PartialEq, Eq)]
enum ToolPauseUpdate {
    Add(String),
    Remove(Vec<String>),
    Clear,
}

fn apply_tool_pause_update(pending: &Arc<Mutex<HashSet<String>>>, event: &RuntimeEvent) {
    let Some(update) = tool_pause_update(event) else {
        return;
    };
    let mut pending = pending.lock().expect("pending tool pauses lock poisoned");
    match update {
        ToolPauseUpdate::Add(tool_use_id) => {
            pending.insert(tool_use_id);
        }
        ToolPauseUpdate::Remove(tool_use_ids) => {
            for tool_use_id in tool_use_ids {
                pending.remove(&tool_use_id);
            }
        }
        ToolPauseUpdate::Clear => pending.clear(),
    }
}

/// 从 runtime payload 提取 pending pause 的增删清空操作。
fn tool_pause_update(event: &RuntimeEvent) -> Option<ToolPauseUpdate> {
    match event
        .payload
        .get("type")
        .and_then(serde_json::Value::as_str)?
    {
        "tool_pause_requested" => event
            .payload
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
            .map(|tool_use_id| ToolPauseUpdate::Add(tool_use_id.to_string())),
        "tool_result" => event
            .payload
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
            .map(|tool_use_id| ToolPauseUpdate::Remove(vec![tool_use_id.to_string()])),
        "subagent_tool_result" => {
            let session_id = event
                .payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)?;
            let tool_use_id = event
                .payload
                .get("tool_result")
                .and_then(|tool_result| tool_result.get("tool_use_id"))
                .and_then(serde_json::Value::as_str)?;
            // 子代理暂停在 UI 中可能用 session_id:tool_use_id 表示，两个 key 都要清掉。
            Some(ToolPauseUpdate::Remove(vec![
                tool_use_id.to_string(),
                format!("{session_id}:{tool_use_id}"),
            ]))
        }
        "run_started" | "run_finished" | "session_changed" => Some(ToolPauseUpdate::Clear),
        _ => None,
    }
}

/// 将 core 配置枚举转成 protocol re-export 的枚举。
fn thinking_effort_to_protocol(
    effort: omini_core::types::config::ThinkingEffort,
) -> protocol::ThinkingEffort {
    match effort {
        omini_core::types::config::ThinkingEffort::None => protocol::ThinkingEffort::None,
        omini_core::types::config::ThinkingEffort::Low => protocol::ThinkingEffort::Low,
        omini_core::types::config::ThinkingEffort::Medium => protocol::ThinkingEffort::Medium,
        omini_core::types::config::ThinkingEffort::High => protocol::ThinkingEffort::High,
    }
}

/// 将数据库会话记录压缩成协议层会话摘要。
fn session_summary_from_store(session: Session) -> protocol::SessionSummary {
    protocol::SessionSummary {
        id: session.id,
        title: session.title.unwrap_or_default(),
        model: session.model,
        provider: session.provider,
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

/// 从首条用户输入生成默认会话标题。
fn initial_session_title_from_input(input: &protocol::UserInput) -> Option<String> {
    let title = input.text.trim();
    (!title.is_empty()).then(|| title.chars().take(300).collect())
}

/// 将持久化 snapshot 转成一组 legacy runtime 事件供 TUI 恢复 UI。
fn session_snapshot_events(
    snapshot: omini_core::types::events::LoadedSession,
    context_window: Option<u32>,
    active_profile: ActiveProfile,
) -> Result<Vec<RuntimeEvent>, CoreError> {
    let mut usage = snapshot.usage;
    usage.context_window = context_window;
    let events = [
        omini_core::types::events::RuntimeToUiEvent::SessionTitleChanged {
            title: snapshot.title,
        },
        omini_core::types::events::RuntimeToUiEvent::ModelChanged {
            provider: snapshot.provider,
            model: snapshot.model,
            thinking_effort: snapshot.thinking_effort,
            context_window,
        },
        omini_core::types::events::RuntimeToUiEvent::ActiveProfileChanged(active_profile),
        omini_core::types::events::RuntimeToUiEvent::SessionChanged {
            session_id: Some(snapshot.session_id),
            messages: snapshot.messages,
            subagents: snapshot.subagents,
            usage,
        },
    ];

    events
        .into_iter()
        .map(runtime_event_from_internal)
        .collect()
}

/// 把 core/TUI 内部事件编码成协议 `RuntimeEvent`。
fn runtime_event_from_internal(
    event: omini_core::types::events::RuntimeToUiEvent,
) -> Result<RuntimeEvent, CoreError> {
    let payload = serde_json::to_value(event)
        .map_err(|error| CoreError::new(format!("Failed to encode runtime event: {error}")))?;
    Ok(RuntimeEvent::new(runtime_event_kind(&payload), payload))
}

/// 从序列化后的 runtime payload 中取出旧协议事件名。
fn runtime_event_kind(payload: &serde_json::Value) -> String {
    payload
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("runtime.event")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_core::types::display::HistoryItem;
    use omini_core::types::events::{
        CompactEvent, CompactSummaryDeltaEvent, CompactTrigger, RuntimeToUiEvent,
        SessionUsageSnapshot, SubmittedPlan,
    };
    use omini_core::types::message::Message;

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

    fn runtime_event(seq: u64, event: RuntimeToUiEvent) -> SequencedRuntimeEvent {
        SequencedRuntimeEvent {
            seq,
            event: runtime_event_from_internal(event).expect("event should encode"),
        }
    }

    fn replay_kinds(buffer: &RuntimeReplayBuffer) -> Vec<String> {
        buffer
            .replay()
            .into_iter()
            .map(|event| event.event.kind)
            .collect()
    }

    fn snapshot(messages: Vec<HistoryItem>) -> LoadedSession {
        LoadedSession {
            session_id: "s1".to_string(),
            provider: "main".to_string(),
            model: "test-model".to_string(),
            thinking_effort: None,
            title: None,
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

    #[tokio::test]
    async fn runtime_load_gate_waiters_follow_successful_loader() {
        let gate = RuntimeLoadGate::default();

        let RuntimeLoadAction::Load = gate.begin_load() else {
            panic!("first caller should load");
        };
        let RuntimeLoadAction::Wait(waiter) = gate.begin_load() else {
            panic!("second caller should wait");
        };

        gate.finish_load(&Ok(()));

        assert!(gate.is_loaded());
        assert_eq!(waiter.await.expect("waiter should receive result"), Ok(()));
        let RuntimeLoadAction::AlreadyLoaded = gate.begin_load() else {
            panic!("loaded gate should stay loaded");
        };
    }

    #[tokio::test]
    async fn runtime_load_gate_error_resets_for_retry() {
        let gate = RuntimeLoadGate::default();

        let RuntimeLoadAction::Load = gate.begin_load() else {
            panic!("first caller should load");
        };
        let RuntimeLoadAction::Wait(waiter) = gate.begin_load() else {
            panic!("second caller should wait");
        };

        let result = Err(CoreError::new("load failed"));
        gate.finish_load(&result);

        assert!(!gate.is_loaded());
        assert_eq!(
            waiter.await.expect("waiter should receive result"),
            Err("load failed".to_string())
        );
        let RuntimeLoadAction::Load = gate.begin_load() else {
            panic!("failed gate should allow retry");
        };
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

    #[test]
    fn replay_buffer_ignores_idle_runtime_events() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "notification"));

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
            RuntimeToUiEvent::PlanSubmitted(SubmittedPlan {
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
            RuntimeToUiEvent::PlanApprovalResolved {
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
        buffer.record(sequenced(2, "session_changed"));
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
        let event =
            runtime_event_from_internal(RuntimeToUiEvent::UserMessageInjected(item.clone()))
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
            event: runtime_event_from_internal(RuntimeToUiEvent::ToolUse(
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
            event: runtime_event_from_internal(RuntimeToUiEvent::ToolResult(tool_result_event))
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
                .map(|event| event.event.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["run_started", "turn_started", "text_delta"]
        );
        assert_eq!(replay[2].event.payload["delta"], "second");
    }

    #[test]
    fn replay_buffer_replays_in_progress_compact_tail_without_run() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(runtime_event(
            1,
            RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
                trigger: CompactTrigger::Manual,
                session_id: Some("s1".to_string()),
                agent_label: None,
            }),
        ));
        buffer.record(runtime_event(
            2,
            RuntimeToUiEvent::CompactSummaryDelta(CompactSummaryDeltaEvent {
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
                .map(|event| event.event.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["compact_summary_started", "compact_summary_delta"]
        );
        assert_eq!(replay[1].event.payload["delta"], "partial");
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
    fn replay_buffer_clears_active_run_on_session_changed() {
        let mut buffer = RuntimeReplayBuffer::default();

        buffer.record(sequenced(1, "run_started"));
        buffer.record(sequenced(2, "turn_started"));
        buffer.record(delta(3, "text_delta", "hello"));
        buffer.record(sequenced(4, "session_changed"));

        assert!(buffer.replay().is_empty());
    }

    #[test]
    fn first_connected_client_becomes_controller() {
        let mut presence = ClientPresence::default();

        let (controller_id, changed) = presence.register("client_1".to_string());

        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_1"));
        assert_eq!(presence.controller_id.as_deref(), Some("client_1"));
    }

    #[test]
    fn second_connected_client_observes_until_takeover() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());

        let (controller_id, changed) = presence.register("client_2".to_string());
        assert!(!changed);
        assert_eq!(controller_id.as_deref(), Some("client_1"));

        let (controller_id, changed) = presence
            .takeover("client_2".to_string())
            .expect("client_2 is connected");
        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_2"));
    }

    #[test]
    fn unconnected_client_cannot_takeover() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());

        let result = presence.takeover("client_2".to_string());

        assert!(result.is_none());
        assert_eq!(presence.controller_id.as_deref(), Some("client_1"));
        assert!(!presence.clients.contains_key("client_2"));
    }

    #[test]
    fn controller_disconnect_promotes_remaining_client() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());
        presence.register("client_2".to_string());

        let (controller_id, changed) = presence.unregister("client_1");

        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_2"));
    }

    #[test]
    fn repeated_connections_keep_client_online_until_last_disconnect() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());
        presence.register("client_1".to_string());
        presence.register("client_2".to_string());

        let (controller_id, changed) = presence.unregister("client_1");
        assert!(!changed);
        assert_eq!(controller_id.as_deref(), Some("client_1"));
        assert!(presence.clients.contains_key("client_1"));

        let (controller_id, changed) = presence.unregister("client_1");
        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_2"));
        assert!(!presence.clients.contains_key("client_1"));
    }

    #[test]
    fn last_disconnect_clears_controller_and_clients() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());

        let (controller_id, changed) = presence.unregister("client_1");

        assert!(changed);
        assert_eq!(controller_id, None);
        assert!(presence.clients.is_empty());
    }

    #[test]
    fn tool_pause_requested_event_adds_pending_resolution_id() {
        let event = RuntimeEvent::new(
            "tool_pause_requested",
            serde_json::json!({
                "type": "tool_pause_requested",
                "tool_use_id": "pause_1",
                "tool_name": "write",
            }),
        );

        assert_eq!(
            tool_pause_update(&event),
            Some(ToolPauseUpdate::Add("pause_1".to_string()))
        );
    }

    #[test]
    fn tool_result_event_removes_pending_resolution_id() {
        let event = RuntimeEvent::new(
            "tool_result",
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": "pause_1",
                "content": "done",
                "is_error": false,
            }),
        );

        assert_eq!(
            tool_pause_update(&event),
            Some(ToolPauseUpdate::Remove(vec!["pause_1".to_string()]))
        );
    }

    #[test]
    fn initial_session_title_from_input_trims_and_limits_text() {
        let input = protocol::UserInput::plain(format!("  {}  ", "a".repeat(400)));

        let title = initial_session_title_from_input(&input).expect("title should be present");

        assert_eq!(title.len(), 300);
        assert!(title.chars().all(|ch| ch == 'a'));
    }

    #[test]
    fn initial_session_title_from_input_skips_blank_text() {
        let input = protocol::UserInput::plain("   ");

        assert_eq!(initial_session_title_from_input(&input), None);
    }

    #[test]
    fn session_snapshot_events_replay_current_session_state() {
        let events = session_snapshot_events(
            LoadedSession {
                session_id: "s1".to_string(),
                provider: "main".to_string(),
                model: "test-model".to_string(),
                thinking_effort: None,
                title: Some("hello".to_string()),
                messages: vec![HistoryItem::Message(Message::from_user_text(
                    "hello".to_string(),
                ))],
                subagents: Vec::new(),
                usage: SessionUsageSnapshot {
                    current_context_tokens: 3,
                    total_tokens: 5,
                    total_cached_tokens: 1,
                    context_window: None,
                },
            },
            Some(1000),
            ActiveProfile::Plan,
        )
        .expect("snapshot events should encode");

        assert_eq!(
            events
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "session_title_changed",
                "model_changed",
                "active_profile_changed",
                "session_changed"
            ]
        );
        assert_eq!(events[0].payload["title"], "hello");
        assert_eq!(events[1].payload["context_window"], 1000);
        assert_eq!(events[2].payload["profile"], "plan");
        assert_eq!(events[3].payload["session_id"], "s1");
        assert_eq!(events[3].payload["usage"]["context_window"], 1000);
        assert_eq!(events[3].payload["messages"].as_array().unwrap().len(), 1);
    }
}
