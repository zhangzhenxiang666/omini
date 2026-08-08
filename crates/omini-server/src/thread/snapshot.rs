use crate::{event::bridge::protocol_events_from_loaded_session_snapshot, thread::ThreadRuntime};
use omini_core::CoreError;
use omini_domain as domain;
use omini_protocol as client_proto;

impl ThreadRuntime {
    pub async fn current_snapshot_events(
        &self,
    ) -> Result<Vec<client_proto::RuntimeEvent>, CoreError> {
        let (snapshot, thread_messages) = self.load_snapshot().await?;
        // snapshot 即将发给客户端，先让 replay buffer 去掉已包含在 snapshot 里的尾部事件。
        // `thread_messages` 来自当前 LLM context，给 LLM 级去重用。
        self.replay_buffer
            .lock()
            .expect("replay buffer lock poisoned")
            .record_snapshot(&snapshot, &thread_messages);
        let context_window = self.context_window_for_snapshot(&snapshot);
        let active_profile = self
            .status_projection
            .lock()
            .expect("status projection lock poisoned")
            .active_profile();
        protocol_events_from_loaded_session_snapshot(snapshot, context_window, active_profile)
    }

    async fn load_snapshot(
        &self,
    ) -> Result<(domain::events::LoadedSession, Vec<domain::message::Message>), CoreError> {
        let thread = self
            .db
            .get_thread(&self.thread_id)
            .await
            .map_err(|error| CoreError::persistence("failed to load thread", error.to_string()))?
            .ok_or(CoreError::ThreadNotFound)?;
        let thread_dir = self.project.thread(&self.thread_id);
        // DB → UI 视角:给 TUI 的 SessionSnapshotEvent 渲染 + user_injection 去重。
        let messages = crate::history::load_messages(&self.db, &self.thread_id, &thread_dir).await;
        // 子代理运行态不随 daemon 存活，DB 加载一律为 Completed；
        // 但当前 daemon 若仍在运行，需要从 runtime 状态投影恢复真实 Running 状态，
        // 否则中途连接的 TUI 无法为仍在运行的子代理加载正确状态。
        let mut subagents =
            crate::history::load_subagents_for_thread(&self.db, &self.thread_id, &self.project)
                .await;
        let running_subagent_ids = self
            .status_projection
            .lock()
            .expect("status projection lock poisoned")
            .running_subagent_thread_ids();
        for subagent in &mut subagents {
            if running_subagent_ids.contains(&subagent.session_id) {
                subagent.status = client_proto::SubagentStatus::Running;
            }
        }
        let active_profile = self
            .status_projection
            .lock()
            .expect("status projection lock poisoned")
            .active_profile();
        let snapshot = domain::events::LoadedSession {
            session_id: thread.id,
            provider: thread.provider.clone(),
            model: thread.model.clone(),
            thinking_effort: {
                let effort = thread
                    .thinking_effort
                    .as_deref()
                    .and_then(|effort| effort.parse().ok());
                self.settings
                    .effective_thinking_effort_for(&thread.provider, &thread.model, effort)
            },
            active_profile,
            title: thread.title,
            messages,
            subagents,
            usage: client_proto::SessionUsageSnapshot {
                current_context_tokens: thread.current_context_tokens,
                total_tokens: thread.total_tokens,
                total_cached_tokens: thread.total_cached_tokens,
                context_window: None,
            },
        };
        let thread_messages = self
            .db
            .load_current_llm_messages(&self.thread_id, &thread_dir)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to load LLM context", error.to_string())
            })?;
        Ok((snapshot, thread_messages))
    }

    fn context_window_for_snapshot(&self, snapshot: &domain::events::LoadedSession) -> Option<u32> {
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
}

#[cfg(test)]
mod tests {
    use omini_config::{ModelEntry, ModelTiers, ProviderConfig, Settings, UserConfig};
    use omini_domain::config::{ProviderEndpointKind, ThinkingEffort};
    use std::collections::HashMap;

    fn test_settings(model: &str) -> Settings {
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
        let config = UserConfig {
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
            model_tiers: ModelTiers::default(),
        };
        config
            .to_settings(Some("openai"), Some(model), None)
            .expect("settings should build")
    }

    fn effective_session_thinking_effort(
        settings: &Settings,
        provider: &str,
        model: &str,
        effort: Option<&str>,
    ) -> Option<ThinkingEffort> {
        let effort = effort.and_then(|effort| effort.parse().ok());
        settings.effective_thinking_effort_for(provider, model, effort)
    }

    #[test]
    fn snapshot_effort_is_cleared_for_non_thinking_model() {
        let settings = test_settings("fast");

        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "fast", Some("medium")),
            None
        );
    }

    #[test]
    fn snapshot_effort_is_kept_for_thinking_model() {
        let settings = test_settings("reasoner");

        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "reasoner", Some("high")),
            Some(ThinkingEffort::High)
        );
        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "reasoner", Some("none")),
            Some(ThinkingEffort::None)
        );
        assert_eq!(
            effective_session_thinking_effort(&settings, "openai", "reasoner", None),
            Some(ThinkingEffort::Medium)
        );
    }
}
