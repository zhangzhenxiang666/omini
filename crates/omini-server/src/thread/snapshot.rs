use crate::{event::bridge::protocol_events_from_loaded_thread_snapshot, thread::ThreadRuntime};
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
        protocol_events_from_loaded_thread_snapshot(snapshot, context_window, active_profile)
    }

    async fn load_snapshot(
        &self,
    ) -> Result<(domain::events::LoadedThread, Vec<domain::message::Message>), CoreError> {
        let thread = self
            .db
            .get_thread(&self.thread_id)
            .await
            .map_err(|error| CoreError::persistence("failed to load thread", error.to_string()))?
            .ok_or(CoreError::ThreadNotFound)?;
        let thread_dir = self.project.thread(&self.thread_id);
        // DB → UI 视角:给 TUI 的 ThreadSnapshotEvent 渲染 + user_injection 去重。
        let messages = crate::history::load_messages(&self.db, &self.thread_id, &thread_dir).await;
        let agent_tasks =
            crate::history::load_agent_tasks_for_thread(&self.db, &self.thread_id, &self.project)
                .await;
        let active_profile = self
            .status_projection
            .lock()
            .expect("status projection lock poisoned")
            .active_profile();
        let snapshot = domain::events::LoadedThread {
            thread_id: thread.id,
            provider: thread.provider.clone(),
            model: thread.model.clone(),
            thinking_effort: {
                let effort = thread
                    .thinking_effort
                    .as_deref()
                    .and_then(|effort| effort.parse().ok());
                self.settings
                    .resolve_model(&omini_config::ModelSelection {
                        active_provider: thread.provider.clone(),
                        model: thread.model.clone(),
                        thinking_effort: effort,
                    })
                    .ok()
                    .and_then(|model| model.thinking_effort)
            },
            active_profile,
            title: thread.title,
            messages,
            agent_tasks,
            usage: client_proto::ThreadUsageSnapshot {
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

    fn context_window_for_snapshot(&self, snapshot: &domain::events::LoadedThread) -> Option<u32> {
        self.settings
            .resolved_config()
            .model(&snapshot.provider, &snapshot.model)
            .ok()
            .map(|model| model.context_window)
    }
}
