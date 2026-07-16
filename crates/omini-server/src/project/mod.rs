//! 项目管理器, 负责项目级别的状态维护以及session的管理

use crate::{session::SessionRuntime, store::Database};
use omini_config::{ConfigError, OminiRoot, UserConfig, project::ProjectDir};
use omini_core::CoreError;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

mod agents;
mod attach;
mod model_selection;
mod sessions;
mod settings;

#[cfg(test)]
mod test_support;

/// 单个项目下的会话管理器。
///
/// 它只缓存当前有客户端使用的 runtime session；持久化会话列表和历史仍来自数据库。
pub struct ProjectManager {
    root: Arc<OminiRoot>,
    cwd: PathBuf,
    project: ProjectDir,
    db: Arc<Database>,
    // 这里只缓存正在被客户端使用的 runtime；空闲后会关闭并从数据库按需恢复。
    sessions: Mutex<HashMap<String, Arc<SessionRuntime>>>,
}

/// 会话查找或恢复过程中可能出现的错误。
#[derive(Debug)]
pub enum SessionError {
    NotFound,
    Core(CoreError),
}

impl From<CoreError> for SessionError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

impl ProjectManager {
    pub fn new(root: Arc<OminiRoot>, cwd: PathBuf, project: ProjectDir, db: Arc<Database>) -> Self {
        Self {
            root,
            cwd,
            project,
            db,
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

pub fn load_validated_config(
    root: &OminiRoot,
    cwd: &std::path::Path,
) -> Result<UserConfig, ConfigError> {
    let config = root.load_config_for_cwd(cwd)?;
    config.validate()?;
    Ok(config)
}
