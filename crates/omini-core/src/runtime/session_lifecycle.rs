use super::service::AgentRuntime;
use super::*;

impl AgentRuntime {
    /// 从 server 提供的持久化快照 hydrate 当前 runtime 状态。
    pub(super) async fn hydrate_session_snapshot(&mut self, snapshot: LoadedSession) {
        self.session_id = Some(snapshot.session_id.clone());

        let session_dir = self.project.session(&snapshot.session_id);
        self.session_dir = Some(session_dir.clone());

        // 同步会话的提供商 / 模型到运行时；若不同则切换。
        let provider_changed = snapshot.provider != self.settings.active_provider
            || snapshot.model != self.settings.model;

        if provider_changed && let Some(profile) = self.settings.providers.get(&snapshot.provider) {
            self.settings.active_provider = snapshot.provider.clone();
            self.settings.model = snapshot.model.clone();
            self.settings.api_key = profile.api_key.clone();
            self.settings.base_url = profile.base_url.clone();
            self.settings.endpoint = profile.endpoint;
            self.llm_client = LlmClient::new(
                profile.endpoint,
                profile.api_key.clone(),
                profile.base_url.clone(),
            );
        }

        // 同步思考强度。
        let thinking_effort = self.settings.effective_thinking_effort_for(
            &snapshot.provider,
            &snapshot.model,
            snapshot.thinking_effort,
        );
        self.settings.thinking_effort = thinking_effort;
        self.set_active_profile(snapshot.active_profile);

        // UI 展示由 server 从 SQLite 提供；LLM 上下文使用 JSONL 历史。
        let runtime_messages = match session_dir.load_history() {
            Ok(messages) => messages,
            Err(e) => {
                self.send_event(RuntimeToServerEvent::warning(format!(
                    "加载 JSONL 历史失败，已降级使用 UI 消息快照: {e}"
                )))
                .await;
                snapshot
                    .messages
                    .iter()
                    .filter_map(|item| match item {
                        HistoryItem::Message(message) => Some(message.clone()),
                        HistoryItem::Display(_)
                        | HistoryItem::Plan(_)
                        | HistoryItem::Summary(_) => None,
                    })
                    .collect()
            }
        };

        self.messages = runtime_messages;
        let mut usage = snapshot.usage;
        usage.context_window = self.current_context_window();
        *self.session_usage.lock().await = usage;

        self.send_event(RuntimeToServerEvent::SessionTitleChanged {
            title: snapshot.title.clone(),
        })
        .await;

        self.send_event(RuntimeToServerEvent::ModelChanged {
            provider: snapshot.provider.clone(),
            model: snapshot.model.clone(),
            thinking_effort,
            context_window: self.current_context_window(),
        })
        .await;

        self.send_event(RuntimeToServerEvent::ActiveProfileChanged(
            self.active_profile(),
        ))
        .await;

        self.send_event(RuntimeToServerEvent::SessionSnapshot {
            session_id: Some(snapshot.session_id),
            messages: snapshot.messages,
            subagents: snapshot.subagents,
            usage,
        })
        .await;
    }

    /// 首次提交时创建 session：生成 UUID、建目录，并把 UI 级索引事件转交给外部 server。
    pub(super) async fn create_session(&mut self, initial_display_message: Option<HistoryItem>) {
        let id = Uuid::new_v4().to_string();
        self.session_id = Some(id.clone());

        let session_dir = self
            .project
            .create_session(&id)
            .expect("failed to create session directory");
        self.session_dir = Some(session_dir);

        self.settings.normalize_current_thinking_effort();

        let now = Utc::now();
        let project_path = sanitize(&self.settings.cwd);
        // 从第一条用户消息中提取标题。
        let title_text =
            history::title_text(initial_display_message.as_ref(), self.messages.last());
        let title = title_text.map(|text| text.chars().take(300).collect());
        let session = SessionRecord {
            id,
            project_path,
            parent_session_id: None,
            spawn_tool_use_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: self.settings.active_provider.clone(),
            model: self.settings.model.clone(),
            thinking_effort: self.settings.thinking_effort.map(|t| t.to_string()),
            title,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            created_at: now,
            updated_at: now,
        };
        if self
            .persistence_tx
            .send(RuntimePersistenceEvent::CreateSession(session.clone()))
            .await
            .is_err()
        {
            self.send_event(RuntimeToServerEvent::error(
                "Failed to forward session persistence event".to_string(),
            ))
            .await;
        }

        let title_out = session.title.clone();
        let session_id_out = session.id.clone();
        let usage = SessionUsageSnapshot {
            context_window: self.current_context_window(),
            ..SessionUsageSnapshot::default()
        };
        *self.session_usage.lock().await = usage;
        self.send_event(RuntimeToServerEvent::SessionTitleChanged { title: title_out })
            .await;
        self.send_event(RuntimeToServerEvent::SessionSnapshot {
            session_id: Some(session_id_out),
            messages: initial_display_message
                .map(|item| vec![item])
                .unwrap_or_else(|| {
                    self.messages
                        .clone()
                        .into_iter()
                        .map(HistoryItem::Message)
                        .collect()
                }),
            subagents: Vec::new(),
            usage,
        })
        .await;
    }

    pub(super) fn current_context_window(&self) -> Option<u32> {
        active_run::current_context_window(&self.settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_config::project::{ProjectDir, ProjectsDir};
    use omini_config::{ModelEntry, ProviderConfig, Settings, UserConfig};
    use omini_domain::config::{ProviderEndpointKind, ThinkingEffort};
    use omini_domain::events::{LoadedSession, SessionUsageSnapshot};
    use omini_runtime_api::RuntimeToServerEvent;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

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
                "omini-core-{test_name}-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            )),
        }
    }

    fn test_config() -> UserConfig {
        let models = HashMap::from([
            (
                "fast".to_string(),
                ModelEntry {
                    name: Some("Fast".to_string()),
                    limit: Some(1000),
                    thinking: Some(false),
                    input_modalities: None,
                    headers: None,
                    body: None,
                },
            ),
            (
                "reasoner".to_string(),
                ModelEntry {
                    name: Some("Reasoner".to_string()),
                    limit: Some(2000),
                    thinking: Some(true),
                    input_modalities: None,
                    headers: None,
                    body: None,
                },
            ),
        ]);

        UserConfig {
            providers: HashMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    name: Some("OpenAI".to_string()),
                    endpoint: ProviderEndpointKind::OpenAI,
                    base_url: "https://openai.example".to_string(),
                    api_key: "test-key".to_string(),
                    models: Some(models),
                },
            )]),
            language: None,
            permissions: None,
            compact: None,
            mcp_servers: HashMap::new(),
        }
    }

    fn settings_for_cwd(cwd: &Path) -> Settings {
        let mut settings = test_config()
            .to_settings(Some("openai"), Some("fast"), None)
            .expect("settings should build");
        settings.cwd = cwd.to_path_buf();
        settings.thinking_effort = Some(ThinkingEffort::Medium);
        settings
    }

    fn project_for(root: &Path, cwd: &Path) -> ProjectDir {
        ProjectsDir::new(root)
            .for_cwd(cwd, &test_config())
            .expect("project should initialize")
    }

    fn runtime_for(
        settings: Settings,
        project: ProjectDir,
    ) -> (
        AgentRuntime,
        mpsc::Receiver<RuntimeToServerEvent>,
        mpsc::Receiver<RuntimePersistenceEvent>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (persistence_tx, persistence_rx) = mpsc::channel(32);
        let (_request_tx, request_rx) = mpsc::channel(1);
        (
            AgentRuntime::new(event_tx, persistence_tx, request_rx, settings, project),
            event_rx,
            persistence_rx,
        )
    }

    #[tokio::test]
    async fn create_session_does_not_persist_effort_for_non_thinking_model() {
        let temp = unique_temp_root("create-session");
        let cwd = temp.path.join("cwd");
        let project = project_for(&temp.path, &cwd);
        let settings = settings_for_cwd(&cwd);
        let (mut runtime, _event_rx, mut persistence_rx) = runtime_for(settings, project);

        runtime.create_session(None).await;

        let event = persistence_rx
            .recv()
            .await
            .expect("create session should persist");
        let RuntimePersistenceEvent::CreateSession(session) = event else {
            panic!("expected CreateSession event");
        };
        assert_eq!(session.model, "fast");
        assert_eq!(session.thinking_effort, None);
        assert_eq!(runtime.settings.thinking_effort, None);
    }

    #[tokio::test]
    async fn hydrate_session_snapshot_does_not_emit_effort_for_non_thinking_model() {
        let temp = unique_temp_root("switch-session");
        let cwd = temp.path.join("cwd");
        let project = project_for(&temp.path, &cwd);
        let settings = settings_for_cwd(&cwd);
        let (mut runtime, mut event_rx, _persistence_rx) = runtime_for(settings, project);
        while event_rx.try_recv().is_ok() {}

        runtime
            .hydrate_session_snapshot(LoadedSession {
                session_id: "s1".to_string(),
                provider: "openai".to_string(),
                model: "fast".to_string(),
                thinking_effort: Some(ThinkingEffort::Medium),
                active_profile: ActiveProfile::Main,
                title: None,
                messages: Vec::new(),
                subagents: Vec::new(),
                usage: SessionUsageSnapshot::default(),
            })
            .await;

        assert_eq!(runtime.settings.thinking_effort, None);
        let mut model_changed = None;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToServerEvent::ModelChanged {
                thinking_effort, ..
            } = event
            {
                model_changed = Some(thinking_effort);
            }
        }
        assert_eq!(model_changed, Some(None));
    }
}
