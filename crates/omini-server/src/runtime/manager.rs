use super::*;

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
#[derive(Debug)]
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
    root: Arc<OminiRoot>,
    db: Arc<Database>,
    // daemon 按项目隔离 SessionManager；HTTP 路由里的 project_id 必须先 attach 才能命中这里。
    projects: Mutex<HashMap<String, Arc<SessionManager>>>,
}

impl GlobalDaemonManager {
    pub(crate) fn new(root: OminiRoot, db: Arc<Database>) -> Self {
        Self {
            root: Arc::new(root),
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

        let config = load_validated_config(&self.root).map_err(ProjectAttachError::Config)?;
        let project = self
            .root
            .init_project(&cwd, &config)
            .map_err(|err| ProjectAttachError::Config(err.to_string()))?;

        let manager = {
            let mut projects = self.projects.lock().expect("projects lock poisoned");
            if let Some(manager) = projects.get(project_id) {
                // 同一项目重复 attach 复用已有 manager，避免拆出多套 session/cache 状态。
                Arc::clone(manager)
            } else {
                let manager = Arc::new(SessionManager::new(
                    Arc::clone(&self.root),
                    cwd.clone(),
                    project,
                    Arc::clone(&self.db),
                ));
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
    root: Arc<OminiRoot>,
    cwd: PathBuf,
    project: ProjectDir,
    db: Arc<Database>,
    // 这里只缓存正在被客户端使用的 runtime；空闲后会关闭并从数据库按需恢复。
    sessions: Mutex<HashMap<String, Arc<RuntimeSession>>>,
}

impl SessionManager {
    pub(crate) fn new(
        root: Arc<OminiRoot>,
        cwd: PathBuf,
        project: ProjectDir,
        db: Arc<Database>,
    ) -> Self {
        Self {
            root,
            cwd,
            project,
            db,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn attach_response(
        &self,
        project_id: &str,
    ) -> Result<protocol::ProjectAttachResponse, CoreError> {
        let settings = self.fresh_settings_with_project_state()?;
        let sessions = self.list_sessions().await?.sessions;
        let context_window = settings.current_model_config().map(|model| model.limit);
        let mcp_server_count = settings
            .mcp_servers
            .values()
            .filter(|server| server.enabled)
            .count();
        let has_project_instructions = self
            .cwd
            .join("AGENTS.md")
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0);
        let show_thinking_blocks = self
            .project
            .load_state()
            .map(|state| state.show_thinking_blocks)
            .unwrap_or(true);
        let agents = omini_core::subagents::list_agent_records(&self.cwd)
            .into_iter()
            .map(|agent| protocol::AgentSummary {
                name: agent.name,
                description: agent.description,
            })
            .collect();
        let skills = omini_core::skills::load_skill_registry(&self.cwd)
            .skills()
            .filter(|skill| skill.user_invocable)
            .map(|skill| protocol::SkillSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect();

        Ok(protocol::ProjectAttachResponse {
            project_id: project_id.to_string(),
            cwd: settings.cwd.display().to_string(),
            sessions,
            active_provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(thinking_effort_to_protocol),
            context_window,
            mcp_server_count,
            has_project_instructions,
            show_thinking_blocks,
            agents,
            skills,
        })
    }

    pub(crate) fn list_models(&self) -> Result<protocol::ModelsResponse, CoreError> {
        Ok(models_response_from_settings(
            &self.fresh_settings_with_project_state()?,
        ))
    }

    pub(crate) fn set_project_model(
        &self,
        request: protocol::SetModelRequest,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let settings = self.fresh_settings_with_project_state()?;
        let provider = settings.providers.get(&request.provider).ok_or_else(|| {
            CoreError::new(format!("Unknown provider profile: {}", request.provider))
        })?;
        if !provider
            .models
            .iter()
            .any(|model| model.id == request.model)
        {
            return Err(CoreError::new(format!(
                "Unknown model '{}' for provider '{}'",
                request.model, request.provider
            )));
        }
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::new(format!("Failed to load project state: {error}")))?;
        let provider = request.provider;
        let model = request.model;
        state.default_provider = Some(provider.clone());
        state.default_model = Some(model.clone());
        state.thinking_effort = settings.effective_thinking_effort_for(
            &provider,
            &model,
            request.thinking_effort.map(thinking_effort_from_protocol),
        );
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::new(format!("Failed to save project state: {error}")))?;
        self.project_runtime_config_response()
    }

    pub(crate) fn set_project_thinking_effort(
        &self,
        request: protocol::SetThinkingEffortRequest,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let settings = self.fresh_settings_with_project_state()?;
        let effort = thinking_effort_from_protocol(request.effort);
        if effort != omini_core::types::config::ThinkingEffort::None
            && !settings.current_model_supports_thinking()
        {
            return Err(CoreError::new(format!(
                "Current model '{}' does not support thinking",
                settings.model
            )));
        }

        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::new(format!("Failed to load project state: {error}")))?;
        state.thinking_effort = settings.effective_current_thinking_effort(Some(effort));
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::new(format!("Failed to save project state: {error}")))?;
        self.project_runtime_config_response()
    }

    pub(crate) fn set_project_thinking_display(
        &self,
        request: protocol::SetThinkingDisplayRequest,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::new(format!("Failed to load project state: {error}")))?;
        state.show_thinking_blocks = request.show.unwrap_or(!state.show_thinking_blocks);
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::new(format!("Failed to save project state: {error}")))?;
        self.project_runtime_config_response()
    }

    fn project_runtime_config_response(
        &self,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let settings = self.fresh_settings_with_project_state()?;
        let show_thinking_blocks = self
            .project
            .load_state()
            .map(|state| state.show_thinking_blocks)
            .unwrap_or(true);
        Ok(protocol::ProjectRuntimeConfigResponse {
            active_provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(thinking_effort_to_protocol),
            context_window: settings.current_model_config().map(|model| model.limit),
            show_thinking_blocks,
        })
    }

    fn fresh_settings_with_project_state(
        &self,
    ) -> Result<omini_core::types::config::Settings, CoreError> {
        let config = load_validated_config(&self.root).map_err(config_core_error)?;
        let state = self
            .project
            .load_state()
            .map_err(|error| CoreError::new(format!("Failed to load project state: {error}")))?;
        let mut settings = config
            .to_settings(
                state.default_provider.as_deref(),
                state.default_model.as_deref(),
                state.thinking_effort,
            )
            .map_err(|error| CoreError::new(format!("Failed to build settings: {error}")))?;
        settings.cwd = self.cwd.clone();
        Ok(settings)
    }

    pub(crate) async fn list_sessions(&self) -> Result<protocol::SessionsResponse, CoreError> {
        let project_path = sanitize(&self.cwd);
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
            .map_err(|error| CoreError::new(format!("Failed to list sessions: {error}")))?;
        let sessions = session_summaries_with_runtime_states(sessions, &runtime_states);
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
        request: protocol::CreateSessionRequest,
    ) -> Result<protocol::CreateSessionResponse, CoreError> {
        let settings = self.settings_for_new_session(&request)?;
        let session_id = uuid::Uuid::new_v4().to_string();
        self.project.create_session(&session_id).map_err(|error| {
            CoreError::new(format!("Failed to create session directory: {error}"))
        })?;
        let now = chrono::Utc::now();
        let session = Session {
            id: session_id.clone(),
            project_path: sanitize(&self.cwd),
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
        self.db
            .create_session(&session)
            .await
            .map_err(|error| CoreError::new(format!("Failed to persist session: {error}")))?;
        let active_profile = request
            .profile
            .map(active_profile_from_protocol)
            .unwrap_or(ActiveProfile::Main);
        let runtime = Arc::new(RuntimeSession::spawn(
            settings,
            self.project.clone(),
            session_id.clone(),
            Arc::clone(&self.db),
            active_profile,
        ));
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .insert(session_id.clone(), runtime);
        Ok(protocol::CreateSessionResponse {
            session_id: Some(session_id),
        })
    }

    fn settings_for_new_session(
        &self,
        request: &protocol::CreateSessionRequest,
    ) -> Result<omini_core::types::config::Settings, CoreError> {
        let mut settings = self.fresh_settings_with_project_state()?;
        if let Some(provider) = &request.provider {
            let profile = settings
                .providers
                .get(provider)
                .ok_or_else(|| CoreError::new(format!("Unknown provider profile: {provider}")))?;
            settings.active_provider = provider.clone();
            settings.api_key = profile.api_key.clone();
            settings.base_url = profile.base_url.clone();
            settings.endpoint = profile.endpoint;
        }
        if let Some(model) = &request.model {
            let provider = settings
                .providers
                .get(&settings.active_provider)
                .ok_or_else(|| {
                    CoreError::new(format!(
                        "Unknown provider profile: {}",
                        settings.active_provider
                    ))
                })?;
            if !provider
                .models
                .iter()
                .any(|candidate| candidate.id == *model)
            {
                return Err(CoreError::new(format!(
                    "Unknown model '{}' for provider '{}'",
                    model, settings.active_provider
                )));
            }
            settings.model = model.clone();
        }
        if let Some(effort) = request.thinking_effort {
            let effort = thinking_effort_from_protocol(effort);
            if effort != omini_core::types::config::ThinkingEffort::None
                && !settings.current_model_supports_thinking()
            {
                return Err(CoreError::new(format!(
                    "Model '{}' does not support thinking",
                    settings.model
                )));
            }
            settings.thinking_effort = settings.effective_current_thinking_effort(Some(effort));
        }
        settings.normalize_current_thinking_effort();
        Ok(settings)
    }

    fn settings_for_existing_session(
        &self,
        session: &Session,
    ) -> Result<omini_core::types::config::Settings, CoreError> {
        let mut settings = self.fresh_settings_with_project_state()?;
        if session.provider.is_empty() || session.model.is_empty() {
            settings.normalize_current_thinking_effort();
            return Ok(settings);
        }

        let (api_key, base_url, endpoint) = {
            let profile = settings.providers.get(&session.provider).ok_or_else(|| {
                CoreError::new(format!("Unknown provider profile: {}", session.provider))
            })?;
            if !profile
                .models
                .iter()
                .any(|candidate| candidate.id == session.model)
            {
                return Err(CoreError::new(format!(
                    "Unknown model '{}' for provider '{}'",
                    session.model, session.provider
                )));
            }
            (
                profile.api_key.clone(),
                profile.base_url.clone(),
                profile.endpoint,
            )
        };

        settings.active_provider = session.provider.clone();
        settings.api_key = api_key;
        settings.base_url = base_url;
        settings.endpoint = endpoint;
        settings.model = session.model.clone();
        let effort: Option<omini_core::types::config::ThinkingEffort> = session
            .thinking_effort
            .as_deref()
            .and_then(|effort| effort.parse().ok());
        settings.thinking_effort = settings.effective_current_thinking_effort(effort);
        settings.normalize_current_thinking_effort();
        Ok(settings)
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

        let project_path = sanitize(&self.cwd);
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

        let settings = self.settings_for_existing_session(&session_record)?;

        // 数据库查询和 runtime 创建之间可能有并发请求，拿到锁后再检查一次缓存。
        let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
        if let Some(session) = sessions.get(session_id) {
            return Ok(Arc::clone(session));
        }
        let session = Arc::new(RuntimeSession::spawn(
            settings,
            self.project.clone(),
            session_id.to_string(),
            Arc::clone(&self.db),
            ActiveProfile::Main,
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

fn load_validated_config(root: &OminiRoot) -> Result<UserConfig, String> {
    let config = root.load_config().map_err(|error| error.to_string())?;
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

fn config_core_error(message: String) -> CoreError {
    CoreError::new(format!("Failed to load user config: {message}"))
}

fn session_summaries_with_runtime_states(
    sessions: Vec<Session>,
    runtime_states: &HashMap<String, protocol::SessionRuntimeState>,
) -> Vec<protocol::SessionSummary> {
    sessions
        .into_iter()
        .map(|session| {
            let mut summary = session_summary_from_store(session);
            summary.runtime_state = runtime_states.get(&summary.id).copied();
            summary
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_core::config::project::ProjectDir;
    use omini_core::types::config::ThinkingEffort;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;

    struct TestRoot {
        path: PathBuf,
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn unique_temp_root(test_name: &str) -> TestRoot {
        TestRoot {
            path: std::env::temp_dir().join(format!(
                "omini-server-{test_name}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            )),
        }
    }

    fn write_config(root: &Path, include_extra_provider: bool) {
        fs::create_dir_all(root).expect("root should be created");
        let mut content = r#"
[providers.openai]
name = "OpenAI"
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models.fast]
name = "Fast"
limit = 1000
thinking = false

[providers.openai.models.reasoner]
name = "Reasoner"
limit = 2000
thinking = true
"#
        .to_string();

        if include_extra_provider {
            content.push_str(
                r#"
[providers.anthropic]
name = "Anthropic"
endpoint = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models.claude-test]
name = "Claude Test"
limit = 3000
thinking = true
"#,
            );
        }

        fs::write(root.join("config.toml"), content).expect("config should be written");
    }

    async fn session_manager_for(root: &Path, cwd: &Path) -> (SessionManager, ProjectDir) {
        write_config(root, false);
        let root = Arc::new(OminiRoot::from_path(root.to_path_buf()));
        let config = load_validated_config(&root).expect("config should load");
        let project = root
            .init_project(cwd, &config)
            .expect("project should initialize");
        let db_path = root.path().join("omini.sqlite");
        let db = Database::open(&db_path)
            .await
            .expect("database should open");
        (
            SessionManager::new(root, cwd.to_path_buf(), project.clone(), Arc::new(db)),
            project,
        )
    }

    fn has_provider(response: &protocol::ModelsResponse, provider: &str) -> bool {
        response
            .providers
            .iter()
            .any(|candidate| candidate.id == provider)
    }

    fn test_session(id: &str) -> Session {
        let now = Utc::now();
        Session {
            id: id.to_string(),
            project_path: "/tmp/project".to_string(),
            parent_session_id: None,
            spawn_tool_use_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            thinking_effort: None,
            title: Some(id.to_string()),
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn session_summaries_merge_loaded_runtime_states() {
        let sessions = vec![test_session("loaded"), test_session("stored")];
        let runtime_states =
            HashMap::from([("loaded".to_string(), protocol::SessionRuntimeState::Working)]);

        let summaries = session_summaries_with_runtime_states(sessions, &runtime_states);

        assert_eq!(
            summaries
                .iter()
                .find(|session| session.id == "loaded")
                .and_then(|session| session.runtime_state),
            Some(protocol::SessionRuntimeState::Working)
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
    async fn project_models_reflect_config_added_after_manager_creation() {
        let temp = unique_temp_root("project-models-refresh");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;

        let initial = manager.list_models().expect("models should load");
        assert!(!has_provider(&initial, "anthropic"));

        write_config(&temp.path, true);

        let refreshed = manager.list_models().expect("models should reload");
        assert!(has_provider(&refreshed, "anthropic"));
    }

    #[tokio::test]
    async fn new_and_restored_sessions_use_latest_config_without_hot_updating_cached_runtime() {
        let temp = unique_temp_root("session-config-refresh");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;

        let old_session_id = manager
            .create_session(protocol::CreateSessionRequest {
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
            .session(&old_session_id)
            .await
            .expect("old runtime should be cached");
        assert!(!has_provider(&old_runtime.core.list_models(), "anthropic"));

        write_config(&temp.path, true);

        assert!(!has_provider(&old_runtime.core.list_models(), "anthropic"));

        let new_session_id = manager
            .create_session(protocol::CreateSessionRequest {
                provider: Some("anthropic".to_string()),
                model: Some("claude-test".to_string()),
                thinking_effort: Some(protocol::ThinkingEffort::High),
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
            .session(&old_session_id)
            .await
            .expect("old session should restore");
        let restored_models = restored.core.list_models();
        assert!(has_provider(&restored_models, "anthropic"));
        assert_eq!(restored_models.current_provider, "openai");
        assert_eq!(restored_models.current_model, "fast");

        restored
            .shutdown()
            .await
            .expect("restored runtime should shut down");
        let new_runtime = manager
            .session(&new_session_id)
            .await
            .expect("new runtime should be cached");
        new_runtime
            .shutdown()
            .await
            .expect("new runtime should shut down");
    }

    #[tokio::test]
    async fn set_project_model_clears_effort_for_non_thinking_model() {
        let temp = unique_temp_root("project-model");
        let cwd = temp.path.join("cwd");
        let (manager, project) = session_manager_for(&temp.path, &cwd).await;
        let mut state = project.load_state().expect("state should load");
        state.thinking_effort = Some(ThinkingEffort::High);
        project.save_state(&state).expect("state should save");

        let response = manager
            .set_project_model(protocol::SetModelRequest {
                provider: "openai".to_string(),
                model: "fast".to_string(),
                thinking_effort: Some(protocol::ThinkingEffort::Medium),
            })
            .expect("model switch should succeed");

        assert_eq!(response.model, "fast");
        assert_eq!(response.thinking_effort, None);
        assert_eq!(
            project
                .load_state()
                .expect("state should load")
                .thinking_effort,
            None
        );
    }

    #[tokio::test]
    async fn set_project_thinking_effort_none_disables_thinking_model_effort() {
        let temp = unique_temp_root("project-effort-none");
        let cwd = temp.path.join("cwd");
        let (manager, project) = session_manager_for(&temp.path, &cwd).await;
        let mut state = project.load_state().expect("state should load");
        state.default_provider = Some("openai".to_string());
        state.default_model = Some("reasoner".to_string());
        state.thinking_effort = Some(ThinkingEffort::High);
        project.save_state(&state).expect("state should save");

        let response = manager
            .set_project_thinking_effort(protocol::SetThinkingEffortRequest {
                effort: protocol::ThinkingEffort::None,
            })
            .expect("none effort should clear");

        assert_eq!(
            response.thinking_effort,
            Some(protocol::ThinkingEffort::None)
        );
        assert_eq!(
            project
                .load_state()
                .expect("state should load")
                .thinking_effort,
            Some(ThinkingEffort::None)
        );
    }
}
