use crate::config::settings::UserConfig;
use crate::types::config::ConfigError;
use crate::types::config::ThinkingEffort;
use crate::types::message::Message;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 将项目路径转义为安全的目录名：`/`、`_`、空格 → `-`
pub fn sanitize(path: &Path) -> String {
    path.to_string_lossy().replace(['/', '_', ' '], "-")
}

/// `~/.omini/projects/` 目录操作句柄。
pub struct ProjectsDir {
    path: PathBuf,
}

impl ProjectsDir {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join("projects"),
        }
    }

    /// 返回 `~/.omini/projects/` 目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 根据当前工作目录获取对应的 `ProjectDir`。
    ///
    /// 将 `cwd` 路径中的 `/`、`_`、空格替换为 `-` 作为目录名。
    /// 如果目录或 `state.toml` 不存在则自动创建并初始化。
    /// 首次创建时会从 `config` 中提取第一个 provider / model 作为默认值。
    /// **每次调用都会刷新 `accessed_at`**，因此适合在启动时调用一次。
    pub fn for_cwd(&self, cwd: &Path, config: &UserConfig) -> Result<ProjectDir, ConfigError> {
        let dirname = sanitize(cwd);
        let project_path = self.path.join(&dirname);
        fs::create_dir_all(&project_path)?;
        let project = ProjectDir { path: project_path };

        if !project.state_path().exists() {
            // 全新项目：提取第一个 provider / model 作为默认值
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
                created_at: now,
                accessed_at: now,
            })?;
        } else {
            // 已有项目：刷新访问时间
            let mut state = project.load_state()?;
            state.accessed_at = chrono::Utc::now();
            project.save_state(&state)?;
        }

        Ok(project)
    }

    /// 列出所有已有的项目目录。
    pub fn list_projects(&self) -> Result<Vec<ProjectDir>, ConfigError> {
        let mut projects = Vec::new();
        if self.path.exists() {
            for entry in fs::read_dir(&self.path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    projects.push(ProjectDir { path: entry.path() });
                }
            }
        }
        Ok(projects)
    }
}

/// `~/.omini/projects/<sanitized-cwd>/` 目录操作句柄。
#[derive(Debug, Clone)]
pub struct ProjectDir {
    path: PathBuf,
}

impl ProjectDir {
    /// 返回项目目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 返回 `state.toml` 路径。
    pub fn state_path(&self) -> PathBuf {
        self.path.join("state.toml")
    }

    /// 返回 `sessions/` 子目录路径。
    pub fn sessions_dir(&self) -> PathBuf {
        self.path.join("sessions")
    }

    /// 加载项目级状态。文件不存在时返回默认值。
    pub fn load_state(&self) -> Result<ProjectState, ConfigError> {
        let path = self.state_path();
        if !path.exists() {
            let now = chrono::Utc::now();
            return Ok(ProjectState {
                default_provider: None,
                default_model: None,
                thinking_effort: None,
                created_at: now,
                accessed_at: now,
            });
        }
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// 保存项目级状态。
    pub fn save_state(&self, state: &ProjectState) -> Result<(), ConfigError> {
        let content = toml::to_string(state)?;
        fs::write(self.state_path(), content)?;
        Ok(())
    }

    /// 获取指定 session 的目录句柄（不创建目录）。
    pub fn session(&self, id: &str) -> SessionDir {
        SessionDir {
            path: self.sessions_dir().join(id),
        }
    }

    /// 创建新 session 目录（含 `sessions/` 父目录）。
    pub fn create_session(&self, id: &str) -> Result<SessionDir, ConfigError> {
        let dir = self.session(id);
        fs::create_dir_all(&dir.path)?;
        Ok(dir)
    }

    /// 列出所有已有的会话目录。
    pub fn list_sessions(&self) -> Result<Vec<SessionDir>, ConfigError> {
        let sessions_path = self.sessions_dir();
        let mut sessions = Vec::new();
        if sessions_path.exists() {
            for entry in fs::read_dir(&sessions_path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    sessions.push(SessionDir { path: entry.path() });
                }
            }
        }
        Ok(sessions)
    }
}

/// `~/.omini/projects/<path>/sessions/<id>/` 目录操作句柄。
#[derive(Debug, Clone)]
pub struct SessionDir {
    path: PathBuf,
}

impl SessionDir {
    /// 返回会话目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 返回 `history.jsonl` 路径。
    pub fn history_path(&self) -> PathBuf {
        self.path.join("history.jsonl")
    }

    /// 获取指定子 agent session 的目录句柄（不创建目录）。
    pub fn subagent(&self, id: &str) -> SessionDir {
        SessionDir {
            path: self.path.join("subagents").join(id),
        }
    }

    /// 创建子 agent session 目录（位于当前 session 的 `subagents/` 下）。
    pub fn create_subagent(&self, id: &str) -> Result<SessionDir, ConfigError> {
        let dir = self.subagent(id);
        fs::create_dir_all(&dir.path)?;
        Ok(dir)
    }

    /// 追加一条 Message 到 `history.jsonl`。
    pub fn append_history(&self, msg: &Message) -> Result<(), ConfigError> {
        let line = serde_json::to_string(msg)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.history_path())?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// 读取全部历史记录（按写入顺序）。
    pub fn load_history(&self) -> Result<Vec<Message>, ConfigError> {
        let path = self.history_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        let mut messages = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let msg: Message = serde_json::from_str(line)?;
            messages.push(msg);
        }
        Ok(messages)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectState {
    /// 当前默认的供应商
    pub default_provider: Option<String>,
    /// 当前默认的模型
    pub default_model: Option<String>,
    /// 当前选择的思考程度（项目级默认）
    pub thinking_effort: Option<ThinkingEffort>,
    /// 项目创建时间
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 最近一次访问时间（每次启动刷新）
    pub accessed_at: chrono::DateTime<chrono::Utc>,
}
