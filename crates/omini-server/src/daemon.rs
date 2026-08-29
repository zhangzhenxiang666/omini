use crate::bundled_tools::BundledTools;
use crate::project::{ProjectManager, load_validated_config};
use crate::store::{Database, Project, StoreError};
use chrono::Utc;
use omini_config::AuthStore;
use omini_config::BootstrapProviderConfig;
use omini_config::ConfigError;
use omini_config::OminiRoot;
use omini_config::bootstrap_global_config;
use omini_config::project::storage_key;
use omini_core::CoreError;
use omini_protocol as protocol;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// daemon 范围内持久化的项目注册表与按需加载的运行时缓存。
pub struct GlobalDaemonManager {
    root: Arc<OminiRoot>,
    db: Arc<Database>,
    projects: Mutex<HashMap<String, Arc<ProjectManager>>>,
    bundled_tools: Arc<BundledTools>,
}

impl GlobalDaemonManager {
    pub fn new(root: OminiRoot, db: Arc<Database>) -> Self {
        let bundled_tools = Arc::new(BundledTools::new(root.path().as_path()));
        Self {
            root: Arc::new(root),
            db,
            projects: Mutex::new(HashMap::new()),
            bundled_tools,
        }
    }

    pub fn bundled_tool_status(&self) -> protocol::BundledToolStatus {
        self.bundled_tools.status()
    }

    pub fn bundled_tools(&self) -> Arc<BundledTools> {
        Arc::clone(&self.bundled_tools)
    }

    pub async fn ensure_bundled_rg(&self) -> Result<(), String> {
        let tools = Arc::clone(&self.bundled_tools);
        tokio::task::spawn_blocking(move || tools.ensure_rg())
            .await
            .map_err(|error| format!("bundled ripgrep restore task failed: {error}"))?
    }

    pub async fn list_projects(&self) -> Result<protocol::ProjectsResponse, ProjectError> {
        let projects = self
            .db
            .list_projects()
            .await?
            .into_iter()
            .map(project_summary)
            .collect();
        Ok(protocol::ProjectsResponse { projects })
    }

    pub async fn register_project(
        &self,
        request: protocol::CreateProjectRequest,
    ) -> Result<protocol::ProjectSummary, ProjectError> {
        let path = canonical_directory(&request.path)?;
        let path_string = path.display().to_string();
        if let Some(project) = self.db.get_project_by_path(&path_string).await? {
            return Ok(project_summary(project));
        }

        let name = match request.name {
            Some(name) => validated_name(&name)?,
            None => default_project_name(&path),
        };
        let id = Uuid::new_v4();
        let storage_key = storage_key(&path, id);
        let now = Utc::now();
        let project = Project {
            id: id.to_string(),
            name,
            path: path_string,
            storage_key,
            created_at: now,
            updated_at: now,
            last_opened_at: None,
        };
        self.db.create_project(&project).await?;
        Ok(project_summary(project))
    }

    pub async fn project_summary(
        &self,
        project_id: &str,
    ) -> Result<protocol::ProjectSummary, ProjectError> {
        let project = self
            .db
            .get_project(project_id)
            .await?
            .ok_or(ProjectError::NotFound)?;
        Ok(project_summary(project))
    }

    pub async fn update_project(
        &self,
        project_id: &str,
        request: protocol::UpdateProjectRequest,
    ) -> Result<protocol::ProjectSummary, ProjectError> {
        if request.name.is_none() && request.path.is_none() {
            return Err(ProjectError::Invalid(
                "Project update must include name or path".to_string(),
            ));
        }
        let mut project = self
            .db
            .get_project(project_id)
            .await?
            .ok_or(ProjectError::NotFound)?;

        if let Some(name) = request.name {
            project.name = validated_name(&name)?;
        }

        let mut relinked = false;
        if let Some(path) = request.path {
            let path = canonical_directory(&path)?;
            let path_string = path.display().to_string();
            if path_string != project.path {
                if let Some(existing) = self.db.get_project_by_path(&path_string).await?
                    && existing.id != project.id
                {
                    return Err(ProjectError::Conflict(format!(
                        "Project path '{}' is already registered",
                        path.display()
                    )));
                }
                if self
                    .projects
                    .lock()
                    .expect("projects lock poisoned")
                    .get(project_id)
                    .is_some_and(|manager| manager.has_active_or_connected_threads())
                {
                    return Err(ProjectError::Conflict(
                        "Project cannot be relinked while a thread is running or connected"
                            .to_string(),
                    ));
                }
                project.path = path_string;
                relinked = true;
            }
        }

        project.updated_at = Utc::now();
        self.db.update_project(&project).await?;
        if relinked {
            self.projects
                .lock()
                .expect("projects lock poisoned")
                .remove(project_id);
        }
        Ok(project_summary(project))
    }

    pub async fn open_project(
        &self,
        project_id: &str,
    ) -> Result<protocol::OpenProjectResponse, ProjectError> {
        let manager = self.get_or_load_project(project_id).await?;
        let project = self.project_summary(project_id).await?;
        let mut response = manager.open_response(project).await?;
        self.db.mark_project_opened(project_id).await?;
        response.project = self.project_summary(project_id).await?;
        Ok(response)
    }

    pub async fn project_configuration(
        &self,
        project_id: &str,
    ) -> Result<protocol::ProjectConfigurationResponse, ProjectError> {
        let project = self
            .db
            .get_project(project_id)
            .await?
            .ok_or(ProjectError::NotFound)?;
        let cwd = PathBuf::from(&project.path);
        if !cwd.is_dir() {
            return Err(ProjectError::MissingPath(project.path));
        }
        Ok(project_configuration_status(&self.root, &cwd))
    }

    pub async fn bootstrap_project_configuration(
        &self,
        project_id: &str,
        request: protocol::BootstrapProjectConfigurationRequest,
    ) -> Result<protocol::ProjectConfigurationResponse, ProjectError> {
        let project = self
            .db
            .get_project(project_id)
            .await?
            .ok_or(ProjectError::NotFound)?;
        let cwd = PathBuf::from(&project.path);
        if !cwd.is_dir() {
            return Err(ProjectError::MissingPath(project.path));
        }
        let current = project_configuration_status(&self.root, &cwd);
        if current.state != protocol::ProjectConfigurationState::SetupRequired {
            return Err(ProjectError::Invalid(
                "Project configuration is not eligible for bootstrap".to_string(),
            ));
        }

        let api_key = request.api_key.filter(|value| !value.trim().is_empty());
        let api_key_env = api_key.as_ref().map(|_| {
            request
                .environment_variable
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| default_api_key_environment_variable(&request.provider_id))
        });
        if let Some(api_key) = api_key {
            let mut auth = AuthStore::load(&self.root.auth_path())
                .map_err(|error| ProjectError::Config(error.to_string()))?;
            auth.upsert_env(
                api_key_env
                    .clone()
                    .expect("api key has an environment variable"),
                api_key,
            )
            .map_err(|error| ProjectError::Config(error.to_string()))?;
            auth.save_atomic(&self.root.auth_path())
                .map_err(|error| ProjectError::Config(error.to_string()))?;
        }

        bootstrap_global_config(
            &self.root,
            &BootstrapProviderConfig {
                provider_id: request.provider_id,
                protocol: request.protocol,
                base_url: request.base_url,
                model_id: request.model_id,
                api_key_env,
            },
        )
        .map_err(|error| ProjectError::Config(error.to_string()))?;

        let status = project_configuration_status(&self.root, &cwd);
        if status.state != protocol::ProjectConfigurationState::Ready {
            return Err(ProjectError::Config(status.message.unwrap_or_else(|| {
                "bootstrap did not produce a valid configuration".to_string()
            })));
        }
        self.projects
            .lock()
            .expect("projects lock poisoned")
            .remove(project_id);
        Ok(status)
    }

    pub async fn get_or_load_project(
        &self,
        project_id: &str,
    ) -> Result<Arc<ProjectManager>, ProjectError> {
        if let Some(project) = self
            .projects
            .lock()
            .expect("projects lock poisoned")
            .get(project_id)
            .cloned()
        {
            return Ok(project);
        }

        let record = self
            .db
            .get_project(project_id)
            .await?
            .ok_or(ProjectError::NotFound)?;
        let cwd = PathBuf::from(&record.path);
        if !cwd.is_dir() {
            return Err(ProjectError::MissingPath(record.path));
        }
        let config = load_validated_config(&self.root, &cwd)
            .map_err(|error| ProjectError::Config(error.to_string()))?;
        let project = self
            .root
            .init_project(&record.storage_key, &config)
            .map_err(|error| ProjectError::Config(error.to_string()))?;
        let loaded = Arc::new(ProjectManager::new(
            record.id.clone(),
            Arc::clone(&self.root),
            cwd,
            project,
            Arc::clone(&self.db),
        ));

        let mut projects = self.projects.lock().expect("projects lock poisoned");
        Ok(Arc::clone(
            projects
                .entry(record.id)
                .or_insert_with(|| Arc::clone(&loaded)),
        ))
    }
}

fn project_configuration_status(
    root: &OminiRoot,
    cwd: &Path,
) -> protocol::ProjectConfigurationResponse {
    match load_validated_config(root, cwd) {
        Ok(_) => protocol::ProjectConfigurationResponse {
            state: protocol::ProjectConfigurationState::Ready,
            code: None,
            message: None,
            provider_id: None,
        },
        Err(ConfigError::NoActiveProvider) => configuration_setup_required(
            "no_provider",
            "No provider is configured for this project",
            None,
        ),
        Err(ConfigError::NoModels(provider_id)) => configuration_setup_required(
            "no_model",
            "A configured provider has no models",
            Some(provider_id),
        ),
        Err(error) => protocol::ProjectConfigurationResponse {
            state: protocol::ProjectConfigurationState::Invalid,
            code: Some(configuration_error_code(&error).to_string()),
            message: Some(error.to_string()),
            provider_id: None,
        },
    }
}

fn configuration_setup_required(
    code: &str,
    message: &str,
    provider_id: Option<String>,
) -> protocol::ProjectConfigurationResponse {
    protocol::ProjectConfigurationResponse {
        state: protocol::ProjectConfigurationState::SetupRequired,
        code: Some(code.to_string()),
        message: Some(message.to_string()),
        provider_id,
    }
}

fn configuration_error_code(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::ConfigParse { .. } | ConfigError::ConfigEdit { .. } => "config_parse_error",
        ConfigError::AuthLoad { .. } | ConfigError::AuthParse { .. } => "auth_error",
        ConfigError::MissingEnv(_) => "missing_environment_variable",
        _ => "invalid_configuration",
    }
}

fn default_api_key_environment_variable(provider_id: &str) -> String {
    let normalized = provider_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{normalized}_API_KEY")
}

#[derive(Debug)]
pub enum ProjectError {
    NotFound,
    Invalid(String),
    Conflict(String),
    MissingPath(String),
    Config(String),
    Store(StoreError),
    Core(CoreError),
}

impl From<StoreError> for ProjectError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<CoreError> for ProjectError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

fn canonical_directory(path: &str) -> Result<PathBuf, ProjectError> {
    if path.trim().is_empty() {
        return Err(ProjectError::Invalid(
            "Project path cannot be empty".to_string(),
        ));
    }
    let path = std::fs::canonicalize(path).map_err(|error| {
        ProjectError::Invalid(format!("Project path cannot be opened: {error}"))
    })?;
    if !path.is_dir() {
        return Err(ProjectError::Invalid(format!(
            "Project path '{}' is not a directory",
            path.display()
        )));
    }
    Ok(path)
}

fn validated_name(name: &str) -> Result<String, ProjectError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ProjectError::Invalid(
            "Project name cannot be empty".to_string(),
        ));
    }
    Ok(name.to_string())
}

fn default_project_name(path: &Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn project_summary(project: Project) -> protocol::ProjectSummary {
    let path_status = if Path::new(&project.path).is_dir() {
        protocol::ProjectPathStatus::Ready
    } else {
        protocol::ProjectPathStatus::Missing
    };
    protocol::ProjectSummary {
        id: project.id,
        name: project.name,
        path: project.path,
        storage_key: project.storage_key,
        path_status,
        created_at: project.created_at,
        updated_at: project.updated_at,
        last_opened_at: project.last_opened_at,
    }
}
