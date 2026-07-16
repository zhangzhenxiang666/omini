use crate::event::bridge::session_summary_from_store_record;
use crate::project::model_selection::{EffortSelection, ModelSelection};
use crate::project::{ProjectManager, SessionError};
use crate::session::SessionRuntime;
use crate::session::SessionRuntimeInputs;
use crate::store::{self as store_model, Database};
use omini_config::Settings;
use omini_config::project::ProjectDir;
use omini_core::CoreError;
use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::Instrument;

impl ProjectManager {
    pub async fn list_sessions(&self) -> Result<client_proto::SessionsResponse, CoreError> {
        let project_path = domain::project::sanitize_project_path(&self.cwd);
        let runtime_states = {
            let sessions = self.sessions.lock().expect("sessions lock poisoned");
            sessions
                .iter()
                .map(|(session_id, session)| (session_id.clone(), session.runtime_state()))
                .collect::<HashMap<_, _>>()
        };
        let sessions = self
            .db
            .list_sessions(&project_path)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to list sessions", error.to_string())
            })?;
        let sessions = session_summaries_with_runtime_states(sessions, &runtime_states);
        Ok(client_proto::SessionsResponse { sessions })
    }

    pub async fn list_session_statuses(
        &self,
        filter: Option<&[client_proto::SessionRuntimeState]>,
    ) -> client_proto::SessionStatusesResponse {
        let mut sessions = {
            let sessions = self.sessions.lock().expect("sessions lock poisoned");
            sessions.values().cloned().collect::<Vec<_>>()
        };
        sessions.sort_by(|left, right| left.session_id().cmp(right.session_id()));

        let mut statuses = Vec::new();
        for session in sessions {
            let status = session.runtime_status();
            let include = filter
                .map(|states| states.contains(&status.state))
                .unwrap_or(true);
            if include {
                statuses.push(status);
            }
        }

        client_proto::SessionStatusesResponse { statuses }
    }

    pub async fn create_session(
        &self,
        request: client_proto::CreateSessionRequest,
    ) -> Result<client_proto::CreateSessionResponse, CoreError> {
        let settings = self.settings_for_model_selection(
            ModelSelection::PartialOverlay {
                provider: request.provider.as_deref(),
                model: request.model.as_deref(),
            },
            EffortSelection::ClientRequest(request.thinking_effort),
        )?;

        let session_id = uuid::Uuid::new_v4().to_string();
        self.project.create_session(&session_id).map_err(|error| {
            CoreError::project_state("failed to create session directory", error)
        })?;
        let now = chrono::Utc::now();
        let session = store_model::Session {
            id: session_id.clone(),
            project_path: domain::project::sanitize_project_path(&self.cwd),
            parent_session_id: None,
            spawn_tool_use_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(|effort| effort.to_string()),
            title: None,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            created_at: now,
            updated_at: now,
        };
        self.db.create_session(&session).await.map_err(|error| {
            CoreError::persistence("failed to persist session", error.to_string())
        })?;
        let active_profile = request
            .profile
            .unwrap_or(domain::events::ActiveProfile::Main);
        // 新建 session 的 jsonl 还没生成,`load_session_snapshot` 走
        // `history::load_messages` 会拿到空 messages,LLM 不会从代码里
        // 看到凭空构造的非空消息。
        let loaded = load_session_snapshot(
            &self.db,
            &self.project,
            &session_id,
            &settings,
            active_profile,
            &session,
        )
        .await?;
        let runtime = Arc::new(SessionRuntime::build(
            settings,
            self.project.clone(),
            session_id.clone(),
            Arc::clone(&self.db),
            active_profile,
            loaded,
        )?);
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .insert(session_id.clone(), runtime);
        Ok(client_proto::CreateSessionResponse {
            session_id: Some(session_id),
        })
    }

    /// 「在新会话中执行计划」审批通过后由 client 调用的 HTTP 路由触发的真正
    /// fork 路径:读 plan 文件 → 构造新 RuntimeSession → 把 plan 包装为
    /// user message 推给新 core → 向老 session 广播 `SessionSwitched`。
    ///
    /// 老 RuntimeSession 不会被强制 shutdown;它由现有 reclaim 机制在所有 client
    /// 断开 + 投影 Idle 后自然回收。
    pub async fn fork_session_for_plan(
        &self,
        from_session_id: &str,
        plan_id: &str,
        profile: client_proto::PlanExecutionProfile,
    ) -> Result<String, CoreError> {
        // 1. 读 plan 文件(由 client HTTP 路由直接调用,不在 core 审批流程中)。
        let plan_path = self
            .project
            .path()
            .join("plans")
            .join(format!("{plan_id}.md"));
        let plan_content = std::fs::read_to_string(&plan_path).map_err(|error| {
            CoreError::new(format!(
                "failed to read plan file for forked session {}: {error}",
                plan_path.display()
            ))
        })?;
        // 2. 构造新 session 所需的 settings(用默认 project state,不从 request 覆盖)。
        let settings = self.fresh_settings_with_state()?;
        // 3. 生成新 session_id、建目录、DB insert(复用 create_session 的部分路径)。
        let new_session_id = uuid::Uuid::new_v4().to_string();
        self.project
            .create_session(&new_session_id)
            .map_err(|error| {
                CoreError::project_state("failed to create session directory", error)
            })?;
        // 新 session 的 title 派生自源 session,加上 "(new from plan)" 后缀,
        // 让历史列表里两个 session 可区分;源 title 为空时退化为只有后缀。
        let source_title = self
            .db
            .get_session(from_session_id)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to load source session title", error.to_string())
            })?
            .and_then(|session| session.title)
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty());
        let new_title = source_title
            .map(|title| format!("{title} (new from plan)"))
            .unwrap_or_else(|| "(new from plan)".to_string());
        let now = chrono::Utc::now();
        let session = store_model::Session {
            id: new_session_id.clone(),
            project_path: domain::project::sanitize_project_path(&self.cwd),
            parent_session_id: None,
            spawn_tool_use_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(|effort| effort.to_string()),
            title: Some(new_title),
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            created_at: now,
            updated_at: now,
        };
        self.db.create_session(&session).await.map_err(|error| {
            CoreError::persistence("failed to persist forked session", error.to_string())
        })?;
        // 4. 构造新 RuntimeSession,active_profile 来自 approval;新建 session
        // 的 messages 同样走 jsonl loader 取(空),保证 LLM 层 messages
        // 一定来源于 jsonl 这条不变量。
        let active_profile = profile.active_profile();
        let loaded = load_session_snapshot(
            &self.db,
            &self.project,
            &new_session_id,
            &settings,
            active_profile,
            &session,
        )
        .await?;
        let runtime = Arc::new(SessionRuntime::build(
            settings,
            self.project.clone(),
            new_session_id.clone(),
            Arc::clone(&self.db),
            active_profile,
            loaded,
        )?);
        // 5. 把 plan 作为新 session 的初始 user message 推给新 core,
        // 走与普通 submit_run 完全相同的路径(包括 process_run 自动启动)。
        let plan_text = omini_core::runtime::compacted_plan_context(&plan_content);
        let submit_command = runtime_contract::session::SubmitRunCommand {
            draft: domain::display::UserDraft::plain(plan_text),
            client_echo_id: None,
            mode: runtime_contract::session::RunInputMode::Submit,
        };
        // 推送失败不致命:RuntimeSession 已建,老 session 状态保持;这里只 log 错误。
        if let Err(error) = runtime.submit_run(submit_command).await {
            tracing::error!(
                error = %error,
                session_id = %new_session_id,
                "forked session runtime failed to consume initial plan message"
            );
        }
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .insert(new_session_id.clone(), runtime);
        // 6. 通过老 session 的 server_event_inbox_tx 广播 SessionSwitched,
        // 走普通 runtime event 通道,所有 ws loop 会向自己的客户端转发
        // `TypedRuntimeEvent::SessionSwitched`。老 session 已被 reclaim
        // (无客户端连接)时跳过——没有接收者,推送无意义。
        if let Some(old) = self.cached_session(from_session_id) {
            old.broadcast_session_switched(from_session_id.to_string(), new_session_id.clone());
        }
        Ok(new_session_id)
    }

    pub fn cached_session(&self, session_id: &str) -> Option<Arc<SessionRuntime>> {
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .get(session_id)
            .cloned()
    }

    pub async fn get_or_load_session(
        &self,
        session_id: &str,
    ) -> Result<Arc<SessionRuntime>, SessionError> {
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

        let project_path = domain::project::sanitize_project_path(&self.cwd);
        let Some(session_record) =
            self.db.get_session(session_id).await.map_err(|error| {
                CoreError::persistence("failed to load session", error.to_string())
            })?
        else {
            return Err(SessionError::NotFound);
        };
        if session_record.project_path != project_path || session_record.parent_session_id.is_some()
        {
            return Err(SessionError::NotFound);
        }

        let (model_selection, effort_selection) =
            if session_record.provider.is_empty() || session_record.model.is_empty() {
                (
                    ModelSelection::ProjectDefault,
                    EffortSelection::InheritProject,
                )
            } else {
                let effort = session_record
                    .thinking_effort
                    .as_deref()
                    .and_then(|effort| effort.parse().ok());

                (
                    ModelSelection::Exact {
                        provider: &session_record.provider,
                        model: &session_record.model,
                    },
                    EffortSelection::PersistedLenient(effort),
                )
            };

        let settings = self.settings_for_model_selection(model_selection, effort_selection)?;

        // 锁外做 jsonl 加载和 `LoadedSession` 组装,messages 走 jsonl loader。
        let loaded = load_session_snapshot(
            &self.db,
            &self.project,
            session_id,
            &settings,
            domain::events::ActiveProfile::Main,
            &session_record,
        )
        .await?;

        // 数据库查询和 runtime 创建之间可能有并发请求，拿到锁后再检查一次缓存。
        if let Some(session) = self
            .sessions
            .lock()
            .expect("sessions lock poisoned")
            .get(session_id)
            .cloned()
        {
            return Ok(session);
        }
        // `build` 本身无 I/O,锁只在 brief get / brief insert 时拿,future 保持 Send。
        let session = Arc::new(SessionRuntime::build(
            settings,
            self.project.clone(),
            session_id.to_string(),
            Arc::clone(&self.db),
            domain::events::ActiveProfile::Main,
            loaded,
        )?);
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .insert(session_id.to_string(), Arc::clone(&session));
        Ok(session)
    }

    pub async fn close_session_if_idle(
        self: &Arc<Self>,
        session_id: &str,
        session: &Arc<SessionRuntime>,
    ) {
        async fn shutdown_session(session: &SessionRuntime) {
            if let Err(error) = session.shutdown().await {
                tracing::warn!(error = %error, "runtime session shutdown failed");
            }
        }

        let mut events = session.subscribe();

        if self.remove_session_if_reclaimable(session_id, session) {
            shutdown_session(session).await;
            return;
        }

        if !self.should_wait_for_reclaim(session_id, session) {
            return;
        }

        let manager = Arc::clone(self);
        let session_id = session_id.to_string();
        let session = Arc::clone(session);
        let watcher_session_id = session_id.clone();
        tokio::spawn(
            async move {
                tracing::debug!("idle reclaim watcher started");
                while matches!(
                    events.recv().await,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_))
                ) {
                    // RunFinished 后可能紧跟 PlanSubmitted。
                    // 先等投影状态稳定，再判断 runtime 是否可回收。
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    if manager.remove_session_if_reclaimable(&session_id, &session) {
                        tracing::debug!("reclaiming idle runtime session");
                        shutdown_session(&session).await;
                        break;
                    }
                    if !manager.should_wait_for_reclaim(&session_id, &session) {
                        break;
                    }
                }
                tracing::debug!("idle reclaim watcher stopped");
            }
            .instrument(tracing::debug_span!(
                "session",
                session_id = %watcher_session_id,
                task_kind = "idle_reclaim_watcher"
            )),
        );
    }

    fn remove_session_if_reclaimable(
        &self,
        session_id: &str,
        session: &Arc<SessionRuntime>,
    ) -> bool {
        if !session.can_reclaim_without_clients() {
            return false;
        }

        let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
        let Some(current) = sessions.get(session_id) else {
            return false;
        };
        // 只关闭当前缓存里的同一个 Arc，避免旧连接清理时误关掉新建 runtime。
        if Arc::ptr_eq(current, session) {
            sessions.remove(session_id);
            true
        } else {
            false
        }
    }

    fn should_wait_for_reclaim(&self, session_id: &str, session: &Arc<SessionRuntime>) -> bool {
        if !session.should_wait_for_reclaim() {
            return false;
        }
        let sessions = self.sessions.lock().expect("sessions lock poisoned");
        sessions
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
    }
}

/// 组装 `LoadedSession`(UI 历史) + LLM 视角的 `Vec<Message>`:
/// - `snapshot.messages` 走 DB 的 `history::load_messages`,带
///   display/plan/summary/normal 全套,给 TUI 的 `SessionSnapshotEvent`
///   渲染 + `UserMessageInjected` 去重(注入可以是任意 kind);
/// - `session_messages` 走 `SessionDir::load_history()`,直读
///   `history.jsonl`,只含真实对话消息,喂给 core / LLM,以及
///   replay buffer 的 LLM 级去重(assistant tail / tool results)。
///
/// 两个数据源严格分开:DB → UI,jsonl → LLM。三个调用点
/// (create_session / fork_session_for_plan / session)都走这个
/// helper,新建 session 时 jsonl 还是空,`load_history` 退化为空 vec,
/// 不会有"在代码里直接 `Vec::new()` 绕开 jsonl"的分支。
async fn load_session_snapshot(
    db: &Database,
    project: &ProjectDir,
    session_id: &str,
    settings: &Settings,
    active_profile: domain::events::ActiveProfile,
    session: &store_model::Session,
) -> Result<SessionRuntimeInputs, CoreError> {
    let session_dir = project.session(session_id);
    let blocks_dir = session_dir.path().join("blocks");
    // DB → UI:全套 HistoryItem(TUI 渲染 + user injection 去重要用)。
    let messages = crate::history::load_messages(db, session_id, &blocks_dir).await;
    let subagents = crate::history::load_subagents_for_session(db, session_id, project).await;
    let snapshot = domain::events::LoadedSession {
        session_id: session.id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        thinking_effort: {
            let effort = session
                .thinking_effort
                .as_deref()
                .and_then(|effort| effort.parse().ok());
            settings.effective_thinking_effort_for(&session.provider, &session.model, effort)
        },
        active_profile,
        title: session.title.clone(),
        messages,
        subagents,
        usage: domain::events::SessionUsageSnapshot {
            current_context_tokens: session.current_context_tokens,
            total_tokens: session.total_tokens,
            total_cached_tokens: session.total_cached_tokens,
            context_window: None,
        },
    };
    // jsonl 损坏(典型场景:异常退出导致末尾半行写入)时降级用 DB Message 子集,记 warn 留痕。
    let session_messages = match session_dir.load_history() {
        Ok(messages) => messages,
        Err(error) => {
            tracing::warn!(
                session_id,
                error = %error,
                "加载 JSONL 历史失败,已降级使用 UI 消息快照中的 Message 子集"
            );
            snapshot
                .messages
                .iter()
                .filter_map(|item| match item {
                    client_proto::HistoryItem::Message(message) => Some(message.clone()),
                    client_proto::HistoryItem::Display(_)
                    | client_proto::HistoryItem::Plan(_)
                    | client_proto::HistoryItem::Summary(_) => None,
                })
                .collect()
        }
    };
    Ok(SessionRuntimeInputs::new(snapshot, session_messages))
}

fn session_summaries_with_runtime_states(
    sessions: Vec<store_model::Session>,
    runtime_states: &HashMap<String, client_proto::SessionRuntimeState>,
) -> Vec<client_proto::SessionSummary> {
    sessions
        .into_iter()
        .map(|session| {
            let mut summary = session_summary_from_store_record(session);
            summary.runtime_state = runtime_states.get(&summary.id).copied();
            summary
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::test_support::{
        has_provider, project_manager_for, recv_runtime_event_kind, test_session, unique_temp_root,
        write_config,
    };
    use omini_domain as domain;
    use omini_protocol as client_proto;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn session_summaries_merge_loaded_runtime_states() {
        let sessions = vec![test_session("loaded"), test_session("stored")];
        let runtime_states = HashMap::from([(
            "loaded".to_string(),
            client_proto::SessionRuntimeState::Working,
        )]);

        let summaries = session_summaries_with_runtime_states(sessions, &runtime_states);

        assert_eq!(
            summaries
                .iter()
                .find(|session| session.id == "loaded")
                .and_then(|session| session.runtime_state),
            Some(client_proto::SessionRuntimeState::Working)
        );
        assert_eq!(
            summaries
                .iter()
                .find(|session| session.id == "stored")
                .and_then(|session| session.runtime_state),
            None
        );
    }

    #[tokio::test]
    async fn new_and_restored_sessions_use_latest_config_without_hot_updating_cached_runtime() {
        let temp = unique_temp_root("session-config-refresh");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;

        let old_session_id = manager
            .create_session(client_proto::CreateSessionRequest {
                provider: Some("openai".to_string()),
                model: Some("fast".to_string()),
                thinking_effort: None,
                profile: None,
            })
            .await
            .expect("old session should be created")
            .session_id
            .expect("session id should be returned");
        let old_runtime = manager
            .get_or_load_session(&old_session_id)
            .await
            .expect("old runtime should be cached");
        assert!(!has_provider(
            &old_runtime.list_models().providers,
            "anthropic"
        ));

        write_config(&temp.path, true);

        assert!(!has_provider(
            &old_runtime.list_models().providers,
            "anthropic"
        ));

        let new_session_id = manager
            .create_session(client_proto::CreateSessionRequest {
                provider: Some("anthropic".to_string()),
                model: Some("claude-test".to_string()),
                thinking_effort: Some(client_proto::ThinkingEffort::High),
                profile: None,
            })
            .await
            .expect("new session should use reloaded config")
            .session_id
            .expect("session id should be returned");
        let new_record = manager
            .db
            .get_session(&new_session_id)
            .await
            .expect("new session should load")
            .expect("new session should exist");
        assert_eq!(new_record.provider, "anthropic");
        assert_eq!(new_record.model, "claude-test");

        let removed = manager
            .sessions
            .lock()
            .expect("sessions lock poisoned")
            .remove(&old_session_id)
            .expect("old runtime should be cached");
        removed
            .shutdown()
            .await
            .expect("old runtime should shut down");

        let restored = manager
            .get_or_load_session(&old_session_id)
            .await
            .expect("old session should restore");
        let restored_models = restored.list_models();
        assert!(has_provider(&restored_models.providers, "anthropic"));
        assert_eq!(restored_models.current_provider, "openai");
        assert_eq!(restored_models.current_model, "fast");

        restored
            .shutdown()
            .await
            .expect("restored runtime should shut down");
        let new_runtime = manager
            .get_or_load_session(&new_session_id)
            .await
            .expect("new runtime should be cached");
        new_runtime
            .shutdown()
            .await
            .expect("new runtime should shut down");
    }

    #[tokio::test]
    async fn close_session_if_idle_keeps_active_runtime_without_clients() {
        let temp = unique_temp_root("idle-active-runtime");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);
        let session_id = manager
            .create_session(client_proto::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .get_or_load_session(&session_id)
            .await
            .expect("session should load");
        session.record_runtime_event_for_test("run_started");

        manager.close_session_if_idle(&session_id, &session).await;

        assert!(
            manager
                .sessions
                .lock()
                .expect("sessions lock poisoned")
                .contains_key(&session_id)
        );
        session.shutdown().await.expect("session should shut down");
    }

    #[tokio::test]
    async fn active_runtime_without_clients_reclaims_after_run_finishes() {
        let temp = unique_temp_root("idle-active-runtime-reclaim");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);
        let session_id = manager
            .create_session(client_proto::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .get_or_load_session(&session_id)
            .await
            .expect("session should load");
        session.record_runtime_event_for_test("run_started");

        manager.close_session_if_idle(&session_id, &session).await;
        session.record_runtime_event_for_test("run_finished");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert!(
            !manager
                .sessions
                .lock()
                .expect("sessions lock poisoned")
                .contains_key(&session_id)
        );
    }

    #[tokio::test]
    async fn fork_session_for_plan_creates_new_runtime_and_broadcasts_session_switched() {
        let temp = unique_temp_root("fork-session-for-plan");
        let cwd = temp.path.join("cwd");
        let (manager, project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let from_session_id = manager
            .create_session(client_proto::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");

        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("plans dir should be created");
        std::fs::write(
            plans_dir.join("plan.md"),
            "# Approved plan\n\n1. Execute it.",
        )
        .expect("plan file should be written");

        let from_session = manager
            .get_or_load_session(&from_session_id)
            .await
            .expect("from session should load");
        let mut events = from_session.subscribe();

        let to_session_id = manager
            .fork_session_for_plan(
                &from_session_id,
                "plan",
                domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect("fork should succeed");
        assert_ne!(to_session_id, from_session_id);

        {
            let sessions = manager.sessions.lock().expect("sessions lock poisoned");
            assert!(sessions.contains_key(&to_session_id));
            assert!(sessions.contains_key(&from_session_id));
        }

        let event = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            recv_runtime_event_kind(&mut events, "session_switched"),
        )
        .await
        .expect("session switch event should arrive within timeout");
        let client_proto::TypedRuntimeEvent::SessionSwitched(client_proto::SessionSwitchedEvent {
            from,
            to,
        }) = event.event.event
        else {
            panic!("expected SessionSwitched typed event");
        };
        assert_eq!(from, from_session_id);
        assert_eq!(to, to_session_id);

        let record = manager
            .db
            .get_session(&to_session_id)
            .await
            .expect("db should load")
            .expect("new session should be persisted");
        assert_eq!(record.id, to_session_id);
        assert_eq!(record.title.as_deref(), Some("(new from plan)"));

        let to_session_dir = project.session(&to_session_id);
        let history = to_session_dir
            .load_history()
            .expect("new session history should load");
        let plan_text =
            omini_core::runtime::compacted_plan_context("# Approved plan\n\n1. Execute it.");
        assert!(
            history.iter().any(|message| {
                message.content.iter().any(|block| match block {
                    domain::message::ContentBlock::Text(text) => text.text == plan_text,
                    _ => false,
                })
            }),
            "forked session jsonl should contain the plan as the first user message"
        );
    }

    #[tokio::test]
    async fn fork_session_for_plan_appends_suffix_to_source_title() {
        let temp = unique_temp_root("fork-session-for-plan-title");
        let cwd = temp.path.join("cwd");
        let (manager, project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let from_session_id = manager
            .create_session(client_proto::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");
        manager
            .db
            .update_session_title(&from_session_id, "refactor auth flow")
            .await
            .expect("source title should update");

        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("plans dir should be created");
        std::fs::write(plans_dir.join("plan.md"), "# plan\n").expect("plan file should be written");

        let to_session_id = manager
            .fork_session_for_plan(
                &from_session_id,
                "plan",
                domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect("fork should succeed");

        let record = manager
            .db
            .get_session(&to_session_id)
            .await
            .expect("db should load")
            .expect("new session should be persisted");
        assert_eq!(
            record.title.as_deref(),
            Some("refactor auth flow (new from plan)")
        );
    }

    #[tokio::test]
    async fn fork_session_for_plan_returns_error_when_plan_file_missing() {
        let temp = unique_temp_root("fork-session-for-plan-missing");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let from_session_id = manager
            .create_session(client_proto::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");

        let error = manager
            .fork_session_for_plan(
                &from_session_id,
                "plan",
                domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect_err("fork should fail when plan file missing");
        let message = error.message();
        assert!(
            message.contains("failed to read plan file"),
            "error message should mention read failure: {message}"
        );

        let sessions = manager.sessions.lock().expect("sessions lock poisoned");
        assert_eq!(
            sessions.len(),
            1,
            "only the original session should exist on failure"
        );
        assert!(sessions.contains_key(&from_session_id));
    }

    #[tokio::test]
    async fn fork_session_for_plan_uses_server_assigned_id_with_no_extra_session_row() {
        let temp = unique_temp_root("fork-session-for-plan-id");
        let cwd = temp.path.join("cwd");
        let (manager, project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);
        let project_path = domain::project::sanitize_project_path(&cwd);

        let from_session_id = manager
            .create_session(client_proto::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");

        let baseline = manager
            .db
            .list_sessions(&project_path)
            .await
            .expect("baseline list should load")
            .len();

        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("plans dir should be created");
        std::fs::write(
            plans_dir.join("plan.md"),
            "# Approved plan\n\n1. Execute it.",
        )
        .expect("plan file should be written");

        let to_session_id = manager
            .fork_session_for_plan(
                &from_session_id,
                "plan",
                domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect("fork should succeed");
        assert_ne!(to_session_id, from_session_id);

        tokio::time::timeout(std::time::Duration::from_millis(500), async {
            loop {
                let current = manager
                    .db
                    .list_sessions(&project_path)
                    .await
                    .expect("list should load")
                    .len();
                if current > baseline {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("new session should be persisted within timeout");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let sessions = manager
            .db
            .list_sessions(&project_path)
            .await
            .expect("post-fork list should load");
        assert_eq!(
            sessions.len(),
            baseline + 1,
            "fork must create exactly one additional session row, not a duplicate from the core"
        );

        let new_session = sessions
            .iter()
            .find(|session| session.id == to_session_id)
            .expect("new session should be in the DB list");
        assert_eq!(new_session.id, to_session_id);

        assert!(
            manager.cached_session(&to_session_id).is_some(),
            "new RuntimeSession should be cached under the server-assigned id"
        );
    }

    #[tokio::test]
    async fn session_load_falls_back_when_history_jsonl_is_corrupt() {
        let temp = unique_temp_root("jsonl-corrupt-fallback");
        let cwd = temp.path.join("cwd");
        let (manager, project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let session_id = manager
            .create_session(client_proto::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");

        let runtime = manager
            .sessions
            .lock()
            .expect("sessions lock poisoned")
            .remove(&session_id)
            .expect("runtime should be cached after create_session");
        runtime.shutdown().await.expect("shutdown should succeed");

        let session_dir = project.session(&session_id);
        std::fs::write(
            session_dir.history_path(),
            "{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}\n\
             {\"role\":\"assistant\",\"content\":[{\"type\":",
        )
        .expect("corrupt jsonl should be written");

        let runtime = manager
            .get_or_load_session(&session_id)
            .await
            .expect("corrupt jsonl should fall back to DB messages, not 500");
        assert!(manager.cached_session(&session_id).is_some());

        runtime.shutdown().await.expect("runtime should shut down");
    }
}
