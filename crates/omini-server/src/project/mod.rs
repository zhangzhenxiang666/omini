//! 项目管理器，负责项目级别的状态维护以及 thread 的管理。

use crate::{store::Store, thread::ThreadSession};
use omini_config::{
    ConfigError, OminiRoot, ResolvedConfig, load_resolved_config_for_cwd, project::ProjectDir,
};
use omini_core::CoreError;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

mod agents;
mod model_selection;
mod open;
mod runs;
mod settings;
mod threads;

#[cfg(test)]
mod test_support;

/// 单个项目下的 thread 管理器。
///
/// 它只缓存当前有客户端使用的会话；持久化列表和历史来自数据库。
pub struct ProjectManager {
    project_id: String,
    root: Arc<OminiRoot>,
    cwd: PathBuf,
    project: ProjectDir,
    db: Arc<Store>,
    // 这里只缓存正在被客户端使用的会话；空闲无客户端后回收并按需从数据库恢复。
    threads: Mutex<HashMap<String, Arc<ThreadSession>>>,
    // 会话消费者公告"空闲可回收"的入口；drain 任务据此摘除缓存。
    idle_reclaim_tx:
        tokio::sync::mpsc::UnboundedSender<(String, omini_core::execution::AgentHandle)>,
    idle_reclaim_rx: Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<(String, omini_core::execution::AgentHandle)>>,
    >,
}

/// thread 查找或恢复过程中可能出现的错误。
#[derive(Debug)]
pub enum ThreadError {
    NotFound,
    Core(CoreError),
}

impl From<CoreError> for ThreadError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

impl ProjectManager {
    pub fn new(
        project_id: String,
        root: Arc<OminiRoot>,
        cwd: PathBuf,
        project: ProjectDir,
        db: Arc<Store>,
    ) -> Self {
        let (idle_reclaim_tx, idle_reclaim_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            project_id,
            root,
            cwd,
            project,
            db,
            threads: Mutex::new(HashMap::new()),
            idle_reclaim_tx,
            idle_reclaim_rx: Mutex::new(Some(idle_reclaim_rx)),
        }
    }

    /// 启动会话回收 drain 任务；必须在进入 tokio 运行时后调用一次。
    ///
    /// 消费者任务在"实例空闲 + 无客户端"时发送会话 ID 并自行关闭实例；
    /// 这里只负责把缓存条目摘除，下一次访问会按需重建会话。
    pub fn start_session_reclaim(self: &Arc<Self>) {
        let Some(mut rx) = self
            .idle_reclaim_rx
            .lock()
            .expect("idle reclaim receiver lock poisoned")
            .take()
        else {
            return;
        };
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            while let Some((thread_id, handle)) = rx.recv().await {
                let mut threads = manager.threads.lock().expect("threads lock poisoned");
                // 摘除即可：实例已由消费者关闭，Arc 落空后消费者任务也随即退出。
                // 老实例的关闭公告不能摘除相同会话 ID 已重建的新实例。
                if threads
                    .get(&thread_id)
                    .is_some_and(|session| session.matches_instance(&handle))
                {
                    threads.remove(&thread_id);
                }
                tracing::debug!(thread_id = %thread_id, "idle session removed from cache");
            }
        });
    }

    pub(crate) fn idle_reclaim_sender(
        &self,
    ) -> tokio::sync::mpsc::UnboundedSender<(String, omini_core::execution::AgentHandle)> {
        self.idle_reclaim_tx.clone()
    }

    pub fn has_active_or_connected_threads(&self) -> bool {
        self.threads
            .lock()
            .expect("threads lock poisoned")
            .values()
            .any(|thread| thread.has_connected_clients() || !thread.is_reclaimable())
    }
}

pub fn load_validated_config(
    root: &OminiRoot,
    cwd: &std::path::Path,
) -> Result<ResolvedConfig, ConfigError> {
    load_resolved_config_for_cwd(root, cwd)
}
