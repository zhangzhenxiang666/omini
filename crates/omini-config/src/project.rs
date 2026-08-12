use crate::settings::ConfigError;
use crate::settings::UserConfig;
use omini_domain::config::ThinkingEffort;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_STORAGE_KEY_BYTES: usize = 240;

/// Builds the stable directory name used below `~/.omini/projects`.
///
/// The readable prefix is only diagnostic. The complete project UUID is the
/// uniqueness boundary and is never truncated.
pub fn storage_key(path: &Path, project_id: Uuid) -> String {
    let mut readable = path
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '.' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if readable.is_empty() {
        readable.push_str("project");
    }

    let suffix = format!("--{project_id}");
    let readable_limit = MAX_STORAGE_KEY_BYTES - suffix.len();
    if readable.len() > readable_limit {
        let mut start = readable.len() - readable_limit;
        while !readable.is_char_boundary(start) {
            start += 1;
        }
        readable = readable[start..].to_string();
    }
    format!("{readable}{suffix}")
}

pub struct ProjectsDir {
    path: PathBuf,
}

impl ProjectsDir {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join("projects"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn for_storage_key(
        &self,
        storage_key: &str,
        config: &UserConfig,
    ) -> Result<ProjectDir, ConfigError> {
        let project_path = self.path.join(storage_key);
        fs::create_dir_all(&project_path)?;
        let project = ProjectDir { path: project_path };

        if !project.state_path().exists() {
            let now = chrono::Utc::now();
            let default_provider = config.providers.keys().next().cloned();
            let default_model = default_provider
                .as_ref()
                .and_then(|name| config.providers.get(name.as_str()))
                .and_then(|pc| pc.models.as_ref())
                .and_then(|models| models.keys().next())
                .cloned();

            project.save_state(&ProjectState {
                default_provider,
                default_model,
                thinking_effort: None,
                show_thinking_blocks: true,
                created_at: now,
                accessed_at: now,
            })?;
        } else {
            let mut state = project.load_state()?;
            state.accessed_at = chrono::Utc::now();
            project.save_state(&state)?;
        }

        Ok(project)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectDir {
    path: PathBuf,
}

impl ProjectDir {
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state_path(&self) -> PathBuf {
        self.path.join("state.toml")
    }

    pub fn threads_dir(&self) -> PathBuf {
        self.path.join("threads")
    }

    pub fn load_state(&self) -> Result<ProjectState, ConfigError> {
        let path = self.state_path();
        if !path.exists() {
            let now = chrono::Utc::now();
            return Ok(ProjectState {
                default_provider: None,
                default_model: None,
                thinking_effort: None,
                show_thinking_blocks: true,
                created_at: now,
                accessed_at: now,
            });
        }
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn save_state(&self, state: &ProjectState) -> Result<(), ConfigError> {
        let content = toml::to_string(state)?;
        fs::write(self.state_path(), content)?;
        Ok(())
    }

    pub fn thread(&self, id: &str) -> ThreadDir {
        ThreadDir {
            path: self.threads_dir().join(id),
        }
    }

    pub fn create_thread(&self, id: &str) -> Result<ThreadDir, ConfigError> {
        let dir = self.thread(id);
        fs::create_dir_all(&dir.path)?;
        fs::create_dir_all(dir.assets_dir())?;
        fs::create_dir_all(dir.sidecars_dir())?;
        fs::create_dir_all(dir.staging_dir())?;
        Ok(dir)
    }

    pub fn list_threads(&self) -> Result<Vec<ThreadDir>, ConfigError> {
        let threads_path = self.threads_dir();
        let mut threads = Vec::new();
        if threads_path.exists() {
            for entry in fs::read_dir(&threads_path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    threads.push(ThreadDir { path: entry.path() });
                }
            }
        }
        Ok(threads)
    }
}

#[derive(Debug, Clone)]
pub struct ThreadDir {
    path: PathBuf,
}

impl ThreadDir {
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.path.join("assets")
    }

    pub fn sidecars_dir(&self) -> PathBuf {
        self.path.join("sidecars")
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.path.join("staging")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectState {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default = "default_show_thinking_blocks")]
    pub show_thinking_blocks: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub accessed_at: chrono::DateTime<chrono::Utc>,
}

fn default_show_thinking_blocks() -> bool {
    true
}
