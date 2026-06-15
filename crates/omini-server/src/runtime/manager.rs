use super::session::load_session_snapshot;
use super::*;
use crate::git;
use omini_domain::display::UserDraft;
use omini_domain::events::PlanExecutionProfile;
use omini_runtime_api::session::{RunInputMode, SubmitRunCommand};
use tracing::Instrument;

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
#[derive(Debug)]
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

        let config = load_validated_config(&self.root, &cwd)
            .map_err(|error| ProjectAttachError::Config(error.to_string()))?;
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

    /// 给后台任务（如后台 session 标题生成）拉取最新项目 settings。
    pub(crate) async fn fresh_settings_with_project_state(
        &self,
        project_id: &str,
    ) -> Result<Settings, CoreError> {
        let project = self.project(project_id).await.map_err(|error| {
            CoreError::new(format!(
                "failed to lookup project {project_id} for background settings load: {error:?}"
            ))
        })?;
        project.fresh_settings_with_project_state()
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
        let agents = omini_core::project_agents_snapshot(&settings)
            .records
            .into_iter()
            .map(|agent| protocol::AgentSummary {
                name: agent.name,
                description: agent.description,
            })
            .collect();
        let skills = omini_core::project_skill_summaries(&self.cwd)
            .into_iter()
            .map(|skill| protocol::SkillSummary {
                name: skill.name,
                description: skill.description,
            })
            .collect();

        let git_branch = git::detect_git_branch(&self.cwd);

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
            git_branch,
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
            CoreError::invalid_model_selection(format!(
                "Unknown provider profile: {}",
                request.provider
            ))
        })?;
        if !provider
            .models
            .iter()
            .any(|model| model.id == request.model)
        {
            return Err(CoreError::invalid_model_selection(format!(
                "Unknown model '{}' for provider '{}'",
                request.model, request.provider
            )));
        }
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
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
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;
        self.project_runtime_config_response()
    }

    pub(crate) fn set_project_thinking_effort(
        &self,
        request: protocol::SetThinkingEffortRequest,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let settings = self.fresh_settings_with_project_state()?;
        let effort = thinking_effort_from_protocol(request.effort);
        if effort != ThinkingEffort::None && !settings.current_model_supports_thinking() {
            return Err(CoreError::invalid_model_selection(format!(
                "Current model '{}' does not support thinking",
                settings.model
            )));
        }

        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        state.thinking_effort = settings.effective_current_thinking_effort(Some(effort));
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;
        self.project_runtime_config_response()
    }

    pub(crate) fn set_project_thinking_display(
        &self,
        request: protocol::SetThinkingDisplayRequest,
    ) -> Result<protocol::ProjectRuntimeConfigResponse, CoreError> {
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        state.show_thinking_blocks = request.show.unwrap_or(!state.show_thinking_blocks);
        self.project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;
        self.project_runtime_config_response()
    }

    pub(crate) fn list_agents(&self) -> Result<protocol::AgentsResponse, CoreError> {
        let settings = self.fresh_settings_with_project_state()?;
        Ok(agents_snapshot_to_protocol(
            omini_core::project_agents_snapshot(&settings),
        ))
    }

    pub(crate) async fn save_agent(
        &self,
        request: protocol::SaveAgentRequest,
        target_session_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let update = omini_core::save_project_agent(
            &self.cwd,
            omini_runtime_api::SaveProjectAgentCommand {
                source_kind: request.source_kind,
                original_agent_id: request.original_agent_id,
                draft: request.draft,
            },
        )?;
        self.refresh_target_session_agents(target_session_id, update.records)
            .await
    }

    pub(crate) async fn delete_agent(
        &self,
        agent_id: &str,
        target_session_id: Option<&str>,
    ) -> Result<(), CoreError> {
        let update = omini_core::delete_project_agent(
            &self.cwd,
            omini_runtime_api::DeleteProjectAgentCommand {
                agent_id: agent_id.to_string(),
            },
        )?;
        self.refresh_target_session_agents(target_session_id, update.records)
            .await
    }

    pub(crate) async fn generate_agent(
        &self,
        request: protocol::GenerateAgentRequest,
    ) -> Result<protocol::GenerateAgentResponse, CoreError> {
        let settings = self.settings_for_agent_generation(&request)?;
        let draft =
            omini_core::generate_project_agent_draft(&settings, &request.description).await?;
        Ok(protocol::GenerateAgentResponse { draft })
    }

    async fn refresh_target_session_agents(
        &self,
        target_session_id: Option<&str>,
        records: Vec<omini_domain::subagents::AgentRecord>,
    ) -> Result<(), CoreError> {
        let Some(session_id) = target_session_id else {
            return Ok(());
        };
        let Some(session) = self.cached_session(session_id) else {
            return Ok(());
        };
        session.reload_subagent_registry().await?;
        session.broadcast_agent_management_updated(records)
    }

    fn cached_session(&self, session_id: &str) -> Option<Arc<RuntimeSession>> {
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .get(session_id)
            .cloned()
    }

    fn settings_for_agent_generation(
        &self,
        request: &protocol::GenerateAgentRequest,
    ) -> Result<Settings, CoreError> {
        let mut settings = self.fresh_settings_with_project_state()?;
        let (api_key, base_url, endpoint) = {
            let provider = settings.providers.get(&request.provider).ok_or_else(|| {
                CoreError::invalid_model_selection(format!(
                    "Unknown provider profile: {}",
                    request.provider
                ))
            })?;
            if !provider
                .models
                .iter()
                .any(|candidate| candidate.id == request.model)
            {
                return Err(CoreError::invalid_model_selection(format!(
                    "Unknown model '{}' for provider '{}'",
                    request.model, request.provider
                )));
            }
            (
                provider.api_key.clone(),
                provider.base_url.clone(),
                provider.endpoint,
            )
        };
        settings.active_provider = request.provider.clone();
        settings.api_key = api_key;
        settings.base_url = base_url;
        settings.endpoint = endpoint;
        settings.model = request.model.clone();
        if let Some(effort) = request.thinking_effort {
            let effort = thinking_effort_from_protocol(effort);
            if effort != ThinkingEffort::None && !settings.current_model_supports_thinking() {
                return Err(CoreError::invalid_model_selection(format!(
                    "Model '{}' does not support thinking",
                    settings.model
                )));
            }
            settings.thinking_effort = settings.effective_current_thinking_effort(Some(effort));
        } else {
            settings.normalize_current_thinking_effort();
        }
        Ok(settings)
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

    pub(crate) fn fresh_settings_with_project_state(&self) -> Result<Settings, CoreError> {
        let config = load_validated_config(&self.root, &self.cwd).map_err(config_core_error)?;
        let state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;
        let mut settings = config
            .to_settings(
                state.default_provider.as_deref(),
                state.default_model.as_deref(),
                state.thinking_effort,
            )
            .map_err(|error| CoreError::config("failed to build settings", error))?;
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
            .map_err(|error| {
                CoreError::persistence("failed to list sessions", error.to_string())
            })?;
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
            CoreError::project_state("failed to create session directory", error)
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
        self.db.create_session(&session).await.map_err(|error| {
            CoreError::persistence("failed to persist session", error.to_string())
        })?;
        let active_profile = request
            .profile
            .map(active_profile_from_protocol)
            .unwrap_or(ActiveProfile::Main);
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
        let runtime = Arc::new(RuntimeSession::build(
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
        Ok(protocol::CreateSessionResponse {
            session_id: Some(session_id),
        })
    }

    /// 「在新会话中执行计划」审批通过后由 client 调用的 HTTP 路由触发的真正
    /// fork 路径:读 plan 文件 → 构造新 RuntimeSession → 把 plan 包装为
    /// user message 推给新 core → 向老 session 广播 `SessionSwitched`。
    ///
    /// 老 RuntimeSession 不会被强制 shutdown;它由现有 reclaim 机制在所有 client
    /// 断开 + 投影 Idle 后自然回收。
    pub(crate) async fn fork_session_for_plan(
        &self,
        from_session_id: &str,
        plan_id: &str,
        profile: PlanExecutionProfile,
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
        let settings = self.fresh_settings_with_project_state()?;
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
        let session = Session {
            id: new_session_id.clone(),
            project_path: sanitize(&self.cwd),
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
        let runtime = Arc::new(RuntimeSession::build(
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
        let submit_command = SubmitRunCommand {
            draft: UserDraft::plain(plan_text),
            client_echo_id: None,
            mode: RunInputMode::Submit,
        };
        // 推送失败不致命:RuntimeSession 已建,老 session 状态保持;这里只 log 错误。
        if let Err(error) = runtime.core.submit_run(submit_command).await {
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

    fn settings_for_new_session(
        &self,
        request: &protocol::CreateSessionRequest,
    ) -> Result<Settings, CoreError> {
        let mut settings = self.fresh_settings_with_project_state()?;
        if let Some(provider) = &request.provider {
            let profile = settings.providers.get(provider).ok_or_else(|| {
                CoreError::invalid_model_selection(format!("Unknown provider profile: {provider}"))
            })?;
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
                    CoreError::invalid_model_selection(format!(
                        "Unknown provider profile: {}",
                        settings.active_provider
                    ))
                })?;
            if !provider
                .models
                .iter()
                .any(|candidate| candidate.id == *model)
            {
                return Err(CoreError::invalid_model_selection(format!(
                    "Unknown model '{}' for provider '{}'",
                    model, settings.active_provider
                )));
            }
            settings.model = model.clone();
        }
        if let Some(effort) = request.thinking_effort {
            let effort = thinking_effort_from_protocol(effort);
            if effort != ThinkingEffort::None && !settings.current_model_supports_thinking() {
                return Err(CoreError::invalid_model_selection(format!(
                    "Model '{}' does not support thinking",
                    settings.model
                )));
            }
            settings.thinking_effort = settings.effective_current_thinking_effort(Some(effort));
        }
        settings.normalize_current_thinking_effort();
        Ok(settings)
    }

    fn settings_for_existing_session(&self, session: &Session) -> Result<Settings, CoreError> {
        let mut settings = self.fresh_settings_with_project_state()?;
        if session.provider.is_empty() || session.model.is_empty() {
            settings.normalize_current_thinking_effort();
            return Ok(settings);
        }

        let (api_key, base_url, endpoint) = {
            let profile = settings.providers.get(&session.provider).ok_or_else(|| {
                CoreError::invalid_model_selection(format!(
                    "Unknown provider profile: {}",
                    session.provider
                ))
            })?;
            if !profile
                .models
                .iter()
                .any(|candidate| candidate.id == session.model)
            {
                return Err(CoreError::invalid_model_selection(format!(
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
        let effort: Option<ThinkingEffort> = session
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

        let settings = self.settings_for_existing_session(&session_record)?;
        // 锁外做 jsonl 加载和 `LoadedSession` 组装,messages 走 jsonl loader。
        let loaded = load_session_snapshot(
            &self.db,
            &self.project,
            session_id,
            &settings,
            ActiveProfile::Main,
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
        let session = Arc::new(RuntimeSession::build(
            settings,
            self.project.clone(),
            session_id.to_string(),
            Arc::clone(&self.db),
            ActiveProfile::Main,
            loaded,
        )?);
        self.sessions
            .lock()
            .expect("sessions lock poisoned")
            .insert(session_id.to_string(), Arc::clone(&session));
        Ok(session)
    }

    pub(crate) async fn close_session_if_idle(
        self: &Arc<Self>,
        session_id: &str,
        session: &Arc<RuntimeSession>,
    ) {
        let mut events = session.subscribe();

        if self.remove_session_if_reclaimable(session_id, session) {
            Self::shutdown_session(session).await;
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
                        Self::shutdown_session(&session).await;
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
        session: &Arc<RuntimeSession>,
    ) -> bool {
        let presence = session.presence.lock().expect("presence lock poisoned");
        if !presence.clients.is_empty() || !session.is_reclaimable() {
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

    fn should_wait_for_reclaim(&self, session_id: &str, session: &Arc<RuntimeSession>) -> bool {
        let presence = session.presence.lock().expect("presence lock poisoned");
        if !presence.clients.is_empty() || session.is_reclaimable() {
            return false;
        }
        let sessions = self.sessions.lock().expect("sessions lock poisoned");
        sessions
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
    }

    async fn shutdown_session(session: &RuntimeSession) {
        if let Err(error) = session.shutdown().await {
            tracing::warn!(error = %error, "runtime session shutdown failed");
        }
    }
}

fn load_validated_config(
    root: &OminiRoot,
    cwd: &std::path::Path,
) -> Result<UserConfig, ConfigError> {
    let config = root.load_config_for_cwd(cwd)?;
    config.validate()?;
    Ok(config)
}

fn config_core_error(error: ConfigError) -> CoreError {
    CoreError::config("failed to load effective config", error)
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
    use omini_config::project::ProjectDir;
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

    fn write_project_config(cwd: &Path) {
        let project_config_dir = cwd.join(".omini");
        fs::create_dir_all(&project_config_dir).expect("project config dir should be created");
        fs::write(
            project_config_dir.join("config.toml"),
            r#"
[providers.anthropic]
name = "Anthropic"
endpoint = "anthropic"
base_url = "https://project-anthropic.example"
api_key = "project-anthropic-key"

[providers.anthropic.models.claude-project]
name = "Claude Project"
limit = 4000
thinking = true
"#,
        )
        .expect("project config should be written");
    }

    async fn session_manager_for(root: &Path, cwd: &Path) -> (SessionManager, ProjectDir) {
        write_config(root, false);
        let root = Arc::new(OminiRoot::from_path(root.to_path_buf()));
        let config = load_validated_config(&root, cwd).expect("config should load");
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

    fn has_provider(providers: &[protocol::ProviderInfo], provider: &str) -> bool {
        providers.iter().any(|candidate| candidate.id == provider)
    }

    async fn recv_runtime_event_kind(
        events: &mut broadcast::Receiver<SequencedRuntimeEvent>,
        kind: &str,
    ) -> SequencedRuntimeEvent {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = events
                    .recv()
                    .await
                    .expect("runtime event should be broadcast");
                if event.event.kind() == kind {
                    return event;
                }
            }
        })
        .await
        .expect("expected runtime event should arrive")
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
        assert!(!has_provider(&initial.providers, "anthropic"));

        write_config(&temp.path, true);

        let refreshed = manager.list_models().expect("models should reload");
        assert!(has_provider(&refreshed.providers, "anthropic"));
    }

    #[tokio::test]
    async fn project_models_include_project_level_config() {
        let temp = unique_temp_root("project-level-config");
        let cwd = temp.path.join("cwd");
        write_config(&temp.path, false);
        write_project_config(&cwd);

        let root = Arc::new(OminiRoot::from_path(temp.path.clone()));
        let config = load_validated_config(&root, &cwd).expect("config should load");
        let project = root
            .init_project(&cwd, &config)
            .expect("project should initialize");
        let db_path = root.path().join("omini.sqlite");
        let db = Database::open(&db_path)
            .await
            .expect("database should open");
        let manager = SessionManager::new(root, cwd, project, Arc::new(db));

        let models = manager.list_models().expect("models should load");

        assert!(has_provider(&models.providers, "openai"));
        assert!(has_provider(&models.providers, "anthropic"));
        let anthropic = models
            .providers
            .iter()
            .find(|provider| provider.id == "anthropic")
            .expect("project provider should be listed");
        assert!(
            anthropic
                .models
                .iter()
                .any(|model| model.id == "claude-project")
        );
    }

    #[tokio::test]
    async fn save_agent_without_target_writes_file_without_spawning_runtime() {
        let temp = unique_temp_root("agent-save-no-target");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;

        manager
            .save_agent(
                protocol::SaveAgentRequest {
                    source_kind: protocol::AgentSourceKind::Project,
                    original_agent_id: None,
                    draft: protocol::AgentDraft {
                        name: "cache-helper".to_string(),
                        description: "Use when checking cache-sensitive changes.".to_string(),
                        instructions: "Inspect cache-sensitive changes.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                None,
            )
            .await
            .expect("agent should save");

        let agents = manager.list_agents().expect("agents should list");
        assert!(
            agents
                .records
                .iter()
                .any(|agent| agent.name == "cache-helper")
        );
        assert!(
            manager
                .sessions
                .lock()
                .expect("sessions lock poisoned")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn save_agent_with_target_notifies_cached_session_agents() {
        let temp = unique_temp_root("agent-save-target");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;
        let session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .session(&session_id)
            .await
            .expect("session should load");
        let mut events = session.subscribe();

        manager
            .save_agent(
                protocol::SaveAgentRequest {
                    source_kind: protocol::AgentSourceKind::Project,
                    original_agent_id: None,
                    draft: protocol::AgentDraft {
                        name: "target-helper".to_string(),
                        description: "Use when testing target refresh.".to_string(),
                        instructions: "Refresh me.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                Some(&session_id),
            )
            .await
            .expect("agent should save");

        let event = recv_runtime_event_kind(&mut events, "agent_management_updated").await;
        assert!(event.seq > 0);
        assert!(matches!(
            event.event.event,
            protocol::TypedRuntimeEvent::AgentManagementUpdated { records }
                if records.iter().any(|record| record.name == "target-helper")
        ));
        session.shutdown().await.expect("session should shut down");
    }

    #[tokio::test]
    async fn save_agent_rejects_built_in_source_kind() {
        let temp = unique_temp_root("agent-save-built-in");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;

        let error = manager
            .save_agent(
                protocol::SaveAgentRequest {
                    source_kind: protocol::AgentSourceKind::BuiltIn,
                    original_agent_id: None,
                    draft: protocol::AgentDraft {
                        name: "bad".to_string(),
                        description: "Bad built-in write.".to_string(),
                        instructions: "Do not write.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                None,
            )
            .await
            .expect_err("built-in writes should fail");

        assert!(error.message().contains("内置 agent 不能写入"));
    }

    #[tokio::test]
    async fn delete_agent_requires_known_editable_record_id() {
        let temp = unique_temp_root("agent-delete-path-ownership");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;

        let arbitrary = cwd.join(".omini").join("agents").join("missing.md");
        let error = manager
            .delete_agent(&arbitrary.display().to_string(), None)
            .await
            .expect_err("unlisted path should not be deletable");
        assert!(error.message().contains("不存在或不可编辑"));

        manager
            .save_agent(
                protocol::SaveAgentRequest {
                    source_kind: protocol::AgentSourceKind::Project,
                    original_agent_id: None,
                    draft: protocol::AgentDraft {
                        name: "deletable".to_string(),
                        description: "Use when testing deletion.".to_string(),
                        instructions: "Delete me.".to_string(),
                        tools: Vec::new(),
                        disallow_tools: Vec::new(),
                        model: None,
                    },
                },
                None,
            )
            .await
            .expect("agent should save");
        let agents = manager.list_agents().expect("agents should list");
        let agent_id = agents
            .records
            .iter()
            .find(|agent| agent.name == "deletable")
            .expect("saved agent should be listed")
            .id
            .clone();

        manager
            .delete_agent(&agent_id, None)
            .await
            .expect("listed editable agent should delete");
        let agents = manager.list_agents().expect("agents should list");
        assert!(!agents.records.iter().any(|agent| agent.name == "deletable"));
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
        assert!(!has_provider(
            &old_runtime.core.list_models().providers,
            "anthropic"
        ));

        write_config(&temp.path, true);

        assert!(!has_provider(
            &old_runtime.core.list_models().providers,
            "anthropic"
        ));

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
        assert!(has_provider(&restored_models.providers, "anthropic"));
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
    async fn close_session_if_idle_keeps_active_runtime_without_clients() {
        let temp = unique_temp_root("idle-active-runtime");
        let cwd = temp.path.join("cwd");
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);
        let session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .session(&session_id)
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
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);
        let session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");
        let session = manager
            .session(&session_id)
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

    #[tokio::test]
    async fn fork_session_for_plan_creates_new_runtime_and_broadcasts_session_switched() {
        let temp = unique_temp_root("fork-session-for-plan");
        let cwd = temp.path.join("cwd");
        let (manager, project) = session_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let from_session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");

        // 写 plans/plan.md 到 project 内部路径,与 server 端 fork 时读取位置一致。
        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("plans dir should be created");
        std::fs::write(
            plans_dir.join("plan.md"),
            "# Approved plan\n\n1. Execute it.",
        )
        .expect("plan file should be written");

        // 订阅老 session 的 runtime event 流,等待 `session_switched` 事件。
        let from_session = manager
            .session(&from_session_id)
            .await
            .expect("from session should load");
        let mut events = from_session.subscribe();

        let to_session_id = manager
            .fork_session_for_plan(
                &from_session_id,
                "plan",
                omini_domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect("fork should succeed");
        assert_ne!(to_session_id, from_session_id);

        // 新 RuntimeSession 注册到 sessions 缓存;老 session 仍保留,由 reclaim 机制自然回收。
        {
            let sessions = manager.sessions.lock().expect("sessions lock poisoned");
            assert!(sessions.contains_key(&to_session_id));
            assert!(sessions.contains_key(&from_session_id));
        }

        // SessionSwitched 必须通过 runtime event 通道广播,from/to 正确。
        let event = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            recv_runtime_event_kind(&mut events, "session_switched"),
        )
        .await
        .expect("session switch event should arrive within timeout");
        let protocol::TypedRuntimeEvent::SessionSwitched(protocol::SessionSwitchedEvent {
            from,
            to,
        }) = event.event.event
        else {
            panic!("expected SessionSwitched typed event");
        };
        assert_eq!(from, from_session_id);
        assert_eq!(to, to_session_id);

        // 新 session 的 DB 行应存在。
        let record = manager
            .db
            .get_session(&to_session_id)
            .await
            .expect("db should load")
            .expect("new session should be persisted");
        assert_eq!(record.id, to_session_id);
        // 源 session 没有 title,新 session 应退化为只有后缀。
        assert_eq!(record.title.as_deref(), Some("(new from plan)"));

        // 新 session 的 jsonl history 文件必须包含 fork 进来的 plan 文本,
        // 证明 fork 完成后 `submit_run` 已落 jsonl(不是"半初始化"状态)。
        let to_session_dir = project.session(&to_session_id);
        let history = to_session_dir
            .load_history()
            .expect("new session history should load");
        let plan_text =
            omini_core::runtime::compacted_plan_context("# Approved plan\n\n1. Execute it.");
        assert!(
            history.iter().any(|message| {
                message.content.iter().any(|block| match block {
                    omini_domain::message::ContentBlock::Text(text) => text.text == plan_text,
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
        let (manager, project) = session_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let from_session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");
        // 给源 session 设一个明确的 title,验证后缀拼接正确。
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
                omini_domain::events::PlanExecutionProfile::Main,
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
        let (manager, _project) = session_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let from_session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
            .await
            .expect("from session should be created")
            .session_id
            .expect("session id should be returned");

        // 故意不写 plans/plan.md。
        let error = manager
            .fork_session_for_plan(
                &from_session_id,
                "plan",
                omini_domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect_err("fork should fail when plan file missing");
        let message = error.message();
        assert!(
            message.contains("failed to read plan file"),
            "error message should mention read failure: {message}"
        );

        // 失败路径不应插入新 session,缓存中只有原 session。
        let sessions = manager.sessions.lock().expect("sessions lock poisoned");
        assert_eq!(
            sessions.len(),
            1,
            "only the original session should exist on failure"
        );
        assert!(sessions.contains_key(&from_session_id));
    }

    /// 回归测试:`fork_session_for_plan` 必须只创建一个新 session,且其 DB 行 id
    /// 与 server 分配给 `RuntimeSession` 缓存键的 id 完全一致。
    ///
    /// 旧的 `AgentRuntime` 设计把 `session_id` 设成 `Option`,core 会在第一次
    /// `submit_run` 时自己再生成一个 UUID 并写一行,导致:
    /// - DB 出现两行(server 分配的空行 + core 自动生成的真实数据行)
    /// - TUI 拿到的 `SessionSnapshot.session_id` 与 `SessionSwitched.to` 不一致
    /// - 后续任何 request 落到 busy 的 core 上都会触发 "Cannot handle this request
    ///   while a run is active"
    ///
    /// 修复后 core 复用 server 分配的 session_id,DB 应只多一行。
    #[tokio::test]
    async fn fork_session_for_plan_uses_server_assigned_id_with_no_extra_session_row() {
        let temp = unique_temp_root("fork-session-for-plan-id");
        let cwd = temp.path.join("cwd");
        let (manager, project) = session_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);
        let project_path = omini_domain::project::sanitize_project_path(&cwd);

        let from_session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
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

        // 写 plans/plan.md,触发成功路径。
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
                omini_domain::events::PlanExecutionProfile::Main,
            )
            .await
            .expect("fork should succeed");
        assert_ne!(to_session_id, from_session_id);

        // 给 core 一点时间消化初始 plan 消息(它会通过 `history::persist_initial_user_message`
        // 写 JSONL,并触发 `UpdateSessionUpdatedAt`;旧 bug 下还会再调一次
        // `create_session` 额外插一行 DB,所以靠"DB 行数 == baseline + 1"能直接抓到)。
        tokio::time::timeout(std::time::Duration::from_millis(500), async {
            loop {
                let current = manager
                    .db
                    .list_sessions(&project_path)
                    .await
                    .expect("list should load")
                    .len();
                if current >= baseline + 1 {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("new session should be persisted within timeout");

        // 给 core 短暂窗口处理初始消息,再断言:DB 只能多出一行。
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

        // 新 session 在 DB 中的 id 必须等于 server 分配给缓存的 id —— 这是修复
        // 之前那个"所有状态嫁接到空会话"bug 的关键断言。
        let new_session = sessions
            .iter()
            .find(|session| session.id == to_session_id)
            .expect("new session should be in the DB list");
        assert_eq!(new_session.id, to_session_id);

        // 新 session 的缓存 key 也必须等于 server 分配的 id,这是 RuntimeSession
        // 用来在 ws 路由上识别 session 的依据。
        assert!(
            manager.cached_session(&to_session_id).is_some(),
            "new RuntimeSession should be cached under the server-assigned id"
        );
    }

    /// 回归测试:jsonl 损坏(末尾半行,模拟异常退出导致写入截断)时,
    /// `manager.session()` 必须降级到 DB messages,而不是返回 500。
    ///
    /// 旧架构下 `hydrate_session_snapshot` 有 match 降级;新架构删了
    /// `RuntimeLoadGate` / `hydrate_session_snapshot` 之后,这个降级也跟着丢了,
    /// 损坏的 jsonl 会让所有 ws 升级请求拿到 500。本测试确保降级路径稳定存在。
    #[tokio::test]
    async fn session_load_falls_back_when_history_jsonl_is_corrupt() {
        let temp = unique_temp_root("jsonl-corrupt-fallback");
        let cwd = temp.path.join("cwd");
        let (manager, project) = session_manager_for(&temp.path, &cwd).await;
        let manager = Arc::new(manager);

        let session_id = manager
            .create_session(protocol::CreateSessionRequest::default())
            .await
            .expect("session should create")
            .session_id
            .expect("session id should be returned");

        // 清掉 `create_session` 缓存的 runtime,让 reload 路径真正走 `load_session_snapshot`。
        manager
            .sessions
            .lock()
            .expect("sessions lock poisoned")
            .remove(&session_id)
            .expect("runtime should be cached after create_session")
            .shutdown()
            .await
            .expect("shutdown should succeed");

        // 模拟异常退出导致的末尾半行写入:第一条完整,第二条是截断的 JSON。
        let session_dir = project.session(&session_id);
        std::fs::write(
            session_dir.history_path(),
            "{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}\n\
             {\"role\":\"assistant\",\"content\":[{\"type\":",
        )
        .expect("corrupt jsonl should be written");

        // 降级路径必须成功;若 `load_session_snapshot` 仍把 jsonl 错误当 500,这里会失败。
        let runtime = manager
            .session(&session_id)
            .await
            .expect("corrupt jsonl should fall back to DB messages, not 500");
        assert!(manager.cached_session(&session_id).is_some());

        runtime.shutdown().await.expect("runtime should shut down");
    }
}
