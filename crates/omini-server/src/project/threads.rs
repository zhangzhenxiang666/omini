use crate::event::bridge::session_summary_from_store_record;
use crate::project::model_selection::{EffortSelection, ModelSelection};
use crate::project::{ProjectManager, ThreadError};
use crate::store::{self as store_model, Database};
use crate::thread::ThreadRuntime;
use crate::thread::ThreadRuntimeInputs;
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
    pub async fn list_threads(&self) -> Result<client_proto::SessionsResponse, CoreError> {
        let runtime_states = {
            let threads = self.threads.lock().expect("threads lock poisoned");
            threads
                .iter()
                .map(|(thread_id, thread)| (thread_id.clone(), thread.runtime_state()))
                .collect::<HashMap<_, _>>()
        };
        let sessions = self
            .db
            .list_threads(&self.project_id)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to list sessions", error.to_string())
            })?;
        let sessions = session_summaries_with_runtime_states(sessions, &runtime_states);
        Ok(client_proto::SessionsResponse { sessions })
    }

    pub async fn list_thread_statuses(
        &self,
        filter: Option<&[client_proto::SessionRuntimeState]>,
    ) -> client_proto::SessionStatusesResponse {
        let mut sessions = {
            let threads = self.threads.lock().expect("threads lock poisoned");
            threads.values().cloned().collect::<Vec<_>>()
        };
        sessions.sort_by(|left, right| left.thread_id().cmp(right.thread_id()));

        let mut statuses = Vec::new();
        for thread in sessions {
            let status = thread.runtime_status();
            let include = filter
                .map(|states| states.contains(&status.state))
                .unwrap_or(true);
            if include {
                statuses.push(status);
            }
        }

        client_proto::SessionStatusesResponse { statuses }
    }

    pub async fn create_thread(
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

        let thread_id = uuid::Uuid::new_v4().to_string();
        self.project.create_thread(&thread_id).map_err(|error| {
            CoreError::project_state("failed to create thread directory", error)
        })?;
        let now = chrono::Utc::now();
        let thread = store_model::Thread {
            id: thread_id.clone(),
            project_id: self.project_id.clone(),
            parent_thread_id: None,
            spawn_tool_use_id: None,
            thread_type: "main".to_string(),
            agent_label: None,
            provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(|effort| effort.to_string()),
            title: None,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            llm_context_version: 1,
            created_at: now,
            updated_at: now,
        };
        self.db.create_thread(&thread).await.map_err(|error| {
            CoreError::persistence("failed to persist thread", error.to_string())
        })?;
        let active_profile = request
            .profile
            .unwrap_or(domain::events::ActiveProfile::Main);
        // 新 thread 在数据库中还没有 UI 或 LLM 消息，因此两个视角都为空。
        let loaded = load_thread_snapshot(
            &self.db,
            &self.project,
            &thread_id,
            &settings,
            active_profile,
            &thread,
        )
        .await?;
        let runtime = Arc::new(ThreadRuntime::build(
            self.project_id.clone(),
            settings,
            self.project.clone(),
            thread_id.clone(),
            Arc::clone(&self.db),
            active_profile,
            loaded,
        )?);
        self.threads
            .lock()
            .expect("threads lock poisoned")
            .insert(thread_id.clone(), runtime);
        Ok(client_proto::CreateSessionResponse {
            session_id: Some(thread_id),
        })
    }

    /// 「在新会话中执行计划」审批通过后由 client 调用的 HTTP 路由触发的真正
    /// fork 路径：读 plan 文件 → 构造新 `ThreadRuntime` → 把 plan 包装为
    /// user message 推给新 core → 向原 thread 广播外部 `SessionSwitched`。
    ///
    /// 原 `ThreadRuntime` 不会被强制 shutdown；它由现有 reclaim 机制在所有 client
    /// 断开 + 投影 Idle 后自然回收。
    pub async fn fork_thread_for_plan(
        &self,
        from_thread_id: &str,
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
        // 2. 构造新 thread 所需的 settings（用默认 project state，不从 request 覆盖）。
        let settings = self.fresh_settings_with_state()?;
        // 3. 生成新 thread id、建目录、写入数据库。
        let new_thread_id = uuid::Uuid::new_v4().to_string();
        self.project
            .create_thread(&new_thread_id)
            .map_err(|error| {
                CoreError::project_state("failed to create thread directory", error)
            })?;
        // 新 thread 的 title 派生自源 thread，加上后缀以便在外部会话列表中区分。
        let source_title = self
            .db
            .get_thread(from_thread_id)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to load source thread title", error.to_string())
            })?
            .and_then(|session| session.title)
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty());
        let new_title = source_title
            .map(|title| format!("{title} (new from plan)"))
            .unwrap_or_else(|| "(new from plan)".to_string());
        let now = chrono::Utc::now();
        let thread = store_model::Thread {
            id: new_thread_id.clone(),
            project_id: self.project_id.clone(),
            parent_thread_id: None,
            spawn_tool_use_id: None,
            thread_type: "main".to_string(),
            agent_label: None,
            provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(|effort| effort.to_string()),
            title: Some(new_title),
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            llm_context_version: 1,
            created_at: now,
            updated_at: now,
        };
        self.db.create_thread(&thread).await.map_err(|error| {
            CoreError::persistence("failed to persist forked thread", error.to_string())
        })?;
        // 4. 构造新 `ThreadRuntime`，active_profile 来自 approval。
        let active_profile = profile.active_profile();
        let loaded = load_thread_snapshot(
            &self.db,
            &self.project,
            &new_thread_id,
            &settings,
            active_profile,
            &thread,
        )
        .await?;
        let runtime = Arc::new(ThreadRuntime::build(
            self.project_id.clone(),
            settings,
            self.project.clone(),
            new_thread_id.clone(),
            Arc::clone(&self.db),
            active_profile,
            loaded,
        )?);
        // 5. 把 plan 作为新 thread 的初始 user message 推给新 core，
        // 走与普通 submit_run 完全相同的路径(包括 process_run 自动启动)。
        let plan_text = omini_core::runtime::compacted_plan_context(&plan_content);
        let submit_command = runtime_contract::thread::SubmitRunCommand {
            draft: domain::display::UserDraft::plain(plan_text),
            client_echo_id: None,
            mode: runtime_contract::thread::RunInputMode::Submit,
        };
        // 推送失败不致命：runtime 已建，原 thread 状态保持；这里只记录错误。
        if let Err(error) = runtime.submit_run(submit_command).await {
            tracing::error!(
                error = %error,
                thread_id = %new_thread_id,
                "forked thread runtime failed to consume initial plan message"
            );
        }
        self.threads
            .lock()
            .expect("threads lock poisoned")
            .insert(new_thread_id.clone(), runtime);
        // 6. 通过原 thread 的 server_event_inbox_tx 广播外部 SessionSwitched，
        // 走普通 runtime event 通道,所有 ws loop 会向自己的客户端转发
        // `TypedRuntimeEvent::SessionSwitched`。原 thread 已被 reclaim
        // (无客户端连接)时跳过——没有接收者,推送无意义。
        if let Some(old) = self.cached_thread(from_thread_id) {
            old.broadcast_session_switched(from_thread_id.to_string(), new_thread_id.clone());
        }
        Ok(new_thread_id)
    }

    pub fn cached_thread(&self, thread_id: &str) -> Option<Arc<ThreadRuntime>> {
        self.threads
            .lock()
            .expect("threads lock poisoned")
            .get(thread_id)
            .cloned()
    }

    pub async fn get_or_load_thread(
        &self,
        thread_id: &str,
    ) -> Result<Arc<ThreadRuntime>, ThreadError> {
        let cached = {
            self.threads
                .lock()
                .expect("threads lock poisoned")
                .get(thread_id)
                .cloned()
        };
        if let Some(thread) = cached {
            return Ok(thread);
        }

        let Some(thread_record) =
            self.db.get_thread(thread_id).await.map_err(|error| {
                CoreError::persistence("failed to load thread", error.to_string())
            })?
        else {
            return Err(ThreadError::NotFound);
        };
        if thread_record.project_id != self.project_id || thread_record.parent_thread_id.is_some() {
            return Err(ThreadError::NotFound);
        }

        let thinking_effort = thread_record
            .thinking_effort
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|()| {
                CoreError::persistence(
                    "failed to load thread",
                    format!(
                        "invalid thinking_effort for thread '{}': {:?}",
                        thread_record.id, thread_record.thinking_effort
                    ),
                )
            })?;
        let settings = self.settings_for_model_selection(
            ModelSelection::Exact {
                provider: &thread_record.provider,
                model: &thread_record.model,
            },
            EffortSelection::ClientRequest(thinking_effort),
        )?;

        // 锁外从数据库加载外部快照与当前 LLM context。
        let loaded = load_thread_snapshot(
            &self.db,
            &self.project,
            thread_id,
            &settings,
            domain::events::ActiveProfile::Main,
            &thread_record,
        )
        .await?;

        // 数据库查询和 runtime 创建之间可能有并发请求，拿到锁后再检查一次缓存。
        if let Some(thread) = self
            .threads
            .lock()
            .expect("threads lock poisoned")
            .get(thread_id)
            .cloned()
        {
            return Ok(thread);
        }
        // `build` 本身无 I/O,锁只在 brief get / brief insert 时拿,future 保持 Send。
        let thread = Arc::new(ThreadRuntime::build(
            self.project_id.clone(),
            settings,
            self.project.clone(),
            thread_id.to_string(),
            Arc::clone(&self.db),
            domain::events::ActiveProfile::Main,
            loaded,
        )?);
        self.threads
            .lock()
            .expect("threads lock poisoned")
            .insert(thread_id.to_string(), Arc::clone(&thread));
        Ok(thread)
    }

    pub async fn close_thread_if_idle(
        self: &Arc<Self>,
        thread_id: &str,
        thread: &Arc<ThreadRuntime>,
    ) {
        async fn shutdown_thread(thread: &ThreadRuntime) {
            if let Err(error) = thread.shutdown().await {
                tracing::warn!(error = %error, "runtime thread shutdown failed");
            }
        }

        let mut events = thread.subscribe();

        if self.remove_thread_if_reclaimable(thread_id, thread) {
            shutdown_thread(thread).await;
            return;
        }

        if !self.should_wait_for_reclaim(thread_id, thread) {
            return;
        }

        let manager = Arc::clone(self);
        let thread_id = thread_id.to_string();
        let thread = Arc::clone(thread);
        let watcher_thread_id = thread_id.clone();
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
                    if manager.remove_thread_if_reclaimable(&thread_id, &thread) {
                        tracing::debug!("reclaiming idle runtime thread");
                        shutdown_thread(&thread).await;
                        break;
                    }
                    if !manager.should_wait_for_reclaim(&thread_id, &thread) {
                        break;
                    }
                }
                tracing::debug!("idle reclaim watcher stopped");
            }
            .instrument(tracing::debug_span!(
                "thread",
                thread_id = %watcher_thread_id,
                task_kind = "idle_reclaim_watcher"
            )),
        );
    }

    fn remove_thread_if_reclaimable(&self, thread_id: &str, thread: &Arc<ThreadRuntime>) -> bool {
        if !thread.can_reclaim_without_clients() {
            return false;
        }

        let mut threads = self.threads.lock().expect("threads lock poisoned");
        let Some(current) = threads.get(thread_id) else {
            return false;
        };
        // 只关闭当前缓存里的同一个 Arc，避免旧连接清理时误关掉新建 runtime。
        if Arc::ptr_eq(current, thread) {
            threads.remove(thread_id);
            true
        } else {
            false
        }
    }

    fn should_wait_for_reclaim(&self, thread_id: &str, thread: &Arc<ThreadRuntime>) -> bool {
        if !thread.should_wait_for_reclaim() {
            return false;
        }
        let threads = self.threads.lock().expect("threads lock poisoned");
        threads
            .get(thread_id)
            .is_some_and(|current| Arc::ptr_eq(current, thread))
    }
}

/// 组装外部 `LoadedSession` 与内部 LLM context。
///
/// UI 历史只从 `messages` 加载；LLM 只加载 `thread.llm_context_version`
/// 指向的 `llm_messages` 快照，两个视角互不回退。
async fn load_thread_snapshot(
    db: &Database,
    project: &ProjectDir,
    thread_id: &str,
    settings: &Settings,
    active_profile: domain::events::ActiveProfile,
    thread: &store_model::Thread,
) -> Result<ThreadRuntimeInputs, CoreError> {
    let thread_dir = project.thread(thread_id);
    // DB → UI:全套 HistoryItem(TUI 渲染 + user injection 去重要用)。
    let messages = crate::history::load_messages(db, thread_id, &thread_dir).await;
    let agent_tasks = crate::history::load_agent_tasks_for_thread(db, thread_id, project).await;
    let snapshot = domain::events::LoadedSession {
        session_id: thread.id.clone(),
        provider: thread.provider.clone(),
        model: thread.model.clone(),
        thinking_effort: settings.thinking_effort,
        active_profile,
        title: thread.title.clone(),
        messages,
        agent_tasks,
        usage: domain::events::SessionUsageSnapshot {
            current_context_tokens: thread.current_context_tokens,
            total_tokens: thread.total_tokens,
            total_cached_tokens: thread.total_cached_tokens,
            context_window: None,
        },
    };
    let thread_messages = db
        .load_current_llm_messages(thread_id, &thread_dir)
        .await
        .map_err(|error| CoreError::persistence("failed to load LLM context", error.to_string()))?;
    Ok(ThreadRuntimeInputs::new(
        snapshot,
        thread_messages,
        thread.llm_context_version,
    ))
}

fn session_summaries_with_runtime_states(
    sessions: Vec<store_model::Thread>,
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
        has_provider, project_manager_for, recv_runtime_event_kind, test_thread, unique_temp_root,
        write_config,
    };
    use omini_domain as domain;
    use omini_protocol as client_proto;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn session_summaries_merge_loaded_runtime_states() {
        let sessions = vec![test_thread("loaded"), test_thread("stored")];
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
            .create_thread(client_proto::CreateSessionRequest {
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
            .get_or_load_thread(&old_session_id)
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
            .create_thread(client_proto::CreateSessionRequest {
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
            .get_thread(&new_session_id)
            .await
            .expect("new session should load")
            .expect("new session should exist");
        assert_eq!(new_record.provider, "anthropic");
        assert_eq!(new_record.model, "claude-test");

        let removed = manager
            .threads
            .lock()
            .expect("sessions lock poisoned")
            .remove(&old_session_id)
            .expect("old runtime should be cached");
        removed
            .shutdown()
            .await
            .expect("old runtime should shut down");

        let restored = manager
            .get_or_load_thread(&old_session_id)
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
            .get_or_load_thread(&new_session_id)
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
            .create_thread(client_proto::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .get_or_load_thread(&session_id)
            .await
            .expect("session should load");
        session.record_runtime_event_for_test("run_started");

        manager.close_thread_if_idle(&session_id, &session).await;

        assert!(
            manager
                .threads
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
            .create_thread(client_proto::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .get_or_load_thread(&session_id)
            .await
            .expect("session should load");
        session.record_runtime_event_for_test("run_started");

        manager.close_thread_if_idle(&session_id, &session).await;
        session.record_runtime_event_for_test("run_finished");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert!(
            !manager
                .threads
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
            .create_thread(client_proto::CreateSessionRequest::default())
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
            .get_or_load_thread(&from_session_id)
            .await
            .expect("from session should load");
        let mut events = from_session.subscribe();

        let to_session_id = manager
            .fork_thread_for_plan(
                &from_session_id,
                "plan",
                domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect("fork should succeed");
        assert_ne!(to_session_id, from_session_id);

        {
            let sessions = manager.threads.lock().expect("threads lock poisoned");
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
            .get_thread(&to_session_id)
            .await
            .expect("db should load")
            .expect("new session should be persisted");
        assert_eq!(record.id, to_session_id);
        assert_eq!(record.title.as_deref(), Some("(new from plan)"));

        let to_session_dir = project.thread(&to_session_id);
        let plan_text =
            omini_core::runtime::compacted_plan_context("# Approved plan\n\n1. Execute it.");
        let history = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            loop {
                let history = manager
                    .db
                    .load_current_llm_messages(&to_session_id, &to_session_dir)
                    .await
                    .expect("new thread LLM context should load");
                if !history.is_empty() {
                    break history;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial plan should be persisted within timeout");
        assert!(
            history.iter().any(|message| {
                message.content.iter().any(|block| match block {
                    domain::message::ContentBlock::Text(text) => text.text == plan_text,
                    _ => false,
                })
            }),
            "forked thread LLM context should contain the plan as the first user message"
        );
    }

    #[tokio::test]
    async fn fork_session_for_plan_appends_suffix_to_source_title() {
        let temp = unique_temp_root("fork-session-for-plan-title");
        let cwd = temp.path.join("cwd");
        let (manager, project) = project_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let from_session_id = manager
            .create_thread(client_proto::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");
        manager
            .db
            .update_thread_title(&from_session_id, "refactor auth flow")
            .await
            .expect("source title should update");

        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("plans dir should be created");
        std::fs::write(plans_dir.join("plan.md"), "# plan\n").expect("plan file should be written");

        let to_session_id = manager
            .fork_thread_for_plan(
                &from_session_id,
                "plan",
                domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect("fork should succeed");

        let record = manager
            .db
            .get_thread(&to_session_id)
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
            .create_thread(client_proto::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");

        let error = manager
            .fork_thread_for_plan(
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

        let sessions = manager.threads.lock().expect("threads lock poisoned");
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
        let project_id = manager.id().to_string();

        let from_session_id = manager
            .create_thread(client_proto::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");

        let baseline = manager
            .db
            .list_threads(&project_id)
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
            .fork_thread_for_plan(
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
                    .list_threads(&project_id)
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
            .list_threads(&project_id)
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
            manager.cached_thread(&to_session_id).is_some(),
            "new ThreadRuntime should be cached under the server-assigned id"
        );
    }
}
