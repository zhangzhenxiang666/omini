//! 项目管理器，负责项目级别的状态维护以及 thread 的管理。

use crate::{store::Database, thread::ThreadRuntime};
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
mod settings;
mod threads;

#[cfg(test)]
mod test_support;

/// 单个项目下的 thread 管理器。
///
/// 它只缓存当前有客户端使用的 runtime thread；持久化列表和历史来自数据库。
pub struct ProjectManager {
    project_id: String,
    root: Arc<OminiRoot>,
    cwd: PathBuf,
    project: ProjectDir,
    db: Arc<Database>,
    // 这里只缓存正在被客户端使用的 runtime；空闲后会关闭并从数据库按需恢复。
    threads: Mutex<HashMap<String, Arc<ThreadRuntime>>>,
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
        db: Arc<Database>,
    ) -> Self {
        Self {
            project_id,
            root,
            cwd,
            project,
            db,
            threads: Mutex::new(HashMap::new()),
        }
    }

    pub fn id(&self) -> &str {
        &self.project_id
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
