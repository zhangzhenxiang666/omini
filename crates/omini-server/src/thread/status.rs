use crate::{event::status::RuntimeStatusSnapshotContext, thread::ThreadSession};
use jiff::Timestamp;
use omini_protocol as client_proto;

impl ThreadSession {
    pub fn runtime_state(&self) -> client_proto::ThreadRuntimeState {
        self.status_projection
            .lock()
            .expect("status projection lock poisoned")
            .state()
    }

    pub fn runtime_status(&self) -> client_proto::ThreadRuntimeStatus {
        let (controller_id, connected_client_count) = {
            let presence = self.presence.lock().expect("presence lock poisoned");
            (
                presence.controller_id.clone(),
                presence.connection_counts.len(),
            )
        };
        // 会话构建即实例启动，暴露给上层时一定处于"已加载"状态。
        let loaded = true;
        let skills = self.handle.runtime_skills();
        let mcp_servers = self.handle.runtime_mcp_servers();
        let subagent_threads = self.handle.runtime_subagents();
        let git_branch = self
            .git_branch
            .lock()
            .expect("git branch cache lock poisoned")
            .clone();
        self.status_projection
            .lock()
            .expect("status projection lock poisoned")
            .to_protocol(
                &self.thread_id,
                RuntimeStatusSnapshotContext {
                    loaded,
                    controller_id,
                    connected_client_count,
                    skills,
                    mcp_servers,
                    subagent_threads: subagent_threads.into_iter().map(Into::into).collect(),
                    now: Timestamp::now(),
                    git_branch,
                },
            )
    }

    /// 以 core 权威快照判断可回收性：无运行、无预留、无未完成任务。
    pub fn is_reclaimable(&self) -> bool {
        self.handle.snapshot().is_reclaimable()
    }

    pub fn can_reclaim_without_clients(&self) -> bool {
        !self.has_connected_clients() && self.is_reclaimable()
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }
}
