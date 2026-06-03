use super::service::AgentRuntime;
use super::*;

impl AgentRuntime {
    /// 切换会话，在 /sessions 交互完成后回调。
    pub(super) async fn switch_session(&mut self, snapshot: LoadedSession) {
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
        let thinking_effort = snapshot.thinking_effort;
        self.settings.thinking_effort = thinking_effort;
        self.set_active_profile(snapshot.active_profile);

        // UI 展示由 server 从 SQLite 提供；LLM 上下文使用 JSONL 历史。
        let runtime_messages = match session_dir.load_history() {
            Ok(messages) => messages,
            Err(e) => {
                self.send_event(RuntimeToUiEvent::warning(format!(
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

        self.send_event(RuntimeToUiEvent::SessionTitleChanged {
            title: snapshot.title.clone(),
        })
        .await;

        self.send_event(RuntimeToUiEvent::ModelChanged {
            provider: snapshot.provider.clone(),
            model: snapshot.model.clone(),
            thinking_effort,
            context_window: self.current_context_window(),
        })
        .await;

        self.send_event(RuntimeToUiEvent::ActiveProfileChanged(
            self.active_profile(),
        ))
        .await;

        self.send_event(RuntimeToUiEvent::SessionChanged {
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
            self.send_event(RuntimeToUiEvent::error(
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
        self.send_event(RuntimeToUiEvent::SessionTitleChanged { title: title_out })
            .await;
        self.send_event(RuntimeToUiEvent::SessionChanged {
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
