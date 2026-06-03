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
        let settings = self.settings_with_project_state();
        let sessions = self.list_sessions().await?.sessions;
        let context_window = settings.current_model_config().map(|model| model.limit);
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
        let agents = omini_core::subagents::list_agent_records(&settings.cwd)
            .into_iter()
            .map(|agent| protocol::AgentSummary {
                name: agent.name,
                description: agent.description,
            })
            .collect();
        let skills = omini_core::skills::load_skill_registry(&settings.cwd)
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

    pub(crate) fn list_models(&self) -> protocol::ModelsResponse {
        models_response_from_settings(&self.settings_with_project_state())
    }

    pub(crate) fn set_project_model(
        &self,
        request: protocol::SetModelRequest,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let provider = self
            .settings
            .providers
            .get(&request.provider)
            .ok_or_else(|| {
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
        state.default_provider = Some(request.provider);
        state.default_model = Some(request.model);
        state.thinking_effort = request.thinking_effort.map(thinking_effort_from_protocol);
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::new(format!("Failed to save project state: {error}")))?;
        Ok(self.project_runtime_config_response())
    }

    pub(crate) fn set_project_thinking_effort(
        &self,
        request: protocol::SetThinkingEffortRequest,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::new(format!("Failed to load project state: {error}")))?;
        state.thinking_effort = Some(thinking_effort_from_protocol(request.effort));
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::new(format!("Failed to save project state: {error}")))?;
        Ok(self.project_runtime_config_response())
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
        Ok(self.project_runtime_config_response())
    }

    fn project_runtime_config_response(&self) -> protocol::ProjectRuntimeConfigResponse {
        let settings = self.settings_with_project_state();
        let show_thinking_blocks = self
            .project
            .load_state()
            .map(|state| state.show_thinking_blocks)
            .unwrap_or(true);
        protocol::ProjectRuntimeConfigResponse {
            active_provider: settings.active_provider.clone(),
            model: settings.model.clone(),
            thinking_effort: settings.thinking_effort.map(thinking_effort_to_protocol),
            context_window: settings.current_model_config().map(|model| model.limit),
            show_thinking_blocks,
        }
    }

    fn settings_with_project_state(&self) -> omini_core::types::config::Settings {
        let mut settings = self.settings.clone();
        let Ok(state) = self.project.load_state() else {
            return settings;
        };
        if let Some(provider) = state.default_provider
            && let Some(profile) = settings.providers.get(&provider)
        {
            settings.active_provider = provider;
            settings.api_key = profile.api_key.clone();
            settings.base_url = profile.base_url.clone();
            settings.endpoint = profile.endpoint;
        }
        if let Some(model) = state.default_model {
            settings.model = model;
        }
        settings.thinking_effort = state.thinking_effort;
        settings
    }

    pub(crate) async fn list_sessions(&self) -> Result<protocol::SessionsResponse, CoreError> {
        let project_path = sanitize(&self.settings.cwd);
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
            project_path: sanitize(&self.settings.cwd),
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
        let mut settings = self.settings_with_project_state();
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
            settings.thinking_effort = Some(thinking_effort_from_protocol(effort));
        }
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
}
