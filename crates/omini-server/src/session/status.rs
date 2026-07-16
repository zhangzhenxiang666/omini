use crate::{event::status::RuntimeStatusSnapshotContext, session::SessionRuntime};
use chrono::Utc;
use omini_protocol as client_proto;

impl SessionRuntime {
    pub fn runtime_state(&self) -> client_proto::SessionRuntimeState {
        self.status_projection
            .lock()
            .expect("status projection lock poisoned")
            .state()
    }

    pub fn runtime_status(&self) -> client_proto::SessionRuntimeStatus {
        let (controller_id, connected_client_count) = {
            let presence = self.presence.lock().expect("presence lock poisoned");
            (presence.controller_id.clone(), presence.clients.len())
        };
        // 新架构下 runtime 启动即加载,RuntimeSession 暴露给上层时一定处于
        // "已加载" 状态,这里直接告诉 status 模块;老架构下的 RuntimeLoadGate
        // 已经不需要再判断。
        let loaded = true;
        let skills = self.core.runtime_skills();
        let mcp_servers = self.core.runtime_mcp_servers();
        let subagent_sessions = self.core.runtime_subagents();
        let git_branch = self
            .git_branch
            .lock()
            .expect("git branch cache lock poisoned")
            .clone();
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
                    subagent_sessions,
                    now: Utc::now(),
                    git_branch,
                },
            )
    }

    pub fn is_reclaimable(&self) -> bool {
        self.runtime_state() == client_proto::SessionRuntimeState::Idle
    }

    pub fn can_reclaim_without_clients(&self) -> bool {
        !self.has_connected_clients() && self.is_reclaimable()
    }

    pub fn should_wait_for_reclaim(&self) -> bool {
        !self.has_connected_clients() && !self.is_reclaimable()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[cfg(test)]
    pub(crate) fn record_runtime_event_for_test(&self, kind: &str) {
        use crate::event::replay::SequencedRuntimeEvent;

        let event = client_proto::RuntimeEvent::new(match kind {
            "run_started" => client_proto::TypedRuntimeEvent::RunStarted,
            "run_finished" => client_proto::TypedRuntimeEvent::RunFinished,
            _ => panic!("unsupported test runtime event kind: {kind}"),
        });
        self.status_projection
            .lock()
            .expect("status projection lock poisoned")
            .record_event(&event, Utc::now());
        let _ = self
            .runtime_event_tx
            .send(SequencedRuntimeEvent { seq: 0, event });
    }
}
