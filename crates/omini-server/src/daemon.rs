use crate::project::{ProjectManager, load_validated_config};
use crate::store::Database;
use omini_config::OminiRoot;
use omini_core::CoreError;
use omini_domain as domain;
use omini_protocol as client_proto;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// daemon 级项目注册表，负责把 project_id 路由到对应的 `ProjectManager`。
pub struct GlobalDaemonManager {
    root: Arc<OminiRoot>,
    db: Arc<Database>,
    // daemon 按项目隔离 ProjectManager；HTTP 路由里的 project_id 必须先 attach 才能命中这里。
    projects: Mutex<HashMap<String, Arc<ProjectManager>>>,
}

impl GlobalDaemonManager {
    pub fn new(root: OminiRoot, db: Arc<Database>) -> Self {
        Self {
            root: Arc::new(root),
            db,
            projects: Mutex::new(HashMap::new()),
        }
    }

    pub async fn attach_project(
        &self,
        project_id: &str,
        cwd: PathBuf,
    ) -> Result<client_proto::ProjectAttachResponse, ProjectAttachError> {
        // project_id 由 cwd 派生，服务端重新计算一次，避免客户端把请求挂到错误项目上。
        let expected_project_id = domain::project::sanitize_project_path(&cwd);
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
                let manager = Arc::new(ProjectManager::new(
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

    pub fn project(&self, project_id: &str) -> Result<Arc<ProjectManager>, ProjectLookupError> {
        self.projects
            .lock()
            .expect("projects lock poisoned")
            .get(project_id)
            .cloned()
            .ok_or(ProjectLookupError::NotFound)
    }
}

/// 项目 attach 入口的错误分类，路由层会映射成协议错误。
pub enum ProjectAttachError {
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
pub enum ProjectLookupError {
    NotFound,
}
