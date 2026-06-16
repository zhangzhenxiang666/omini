use crate::settings::ConfigError;
use crate::settings::UserConfig;
use omini_domain::config::ThinkingEffort;
use omini_domain::message::Message;
use omini_domain::project::sanitize_project_path as sanitize;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

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

    pub fn for_cwd(&self, cwd: &Path, config: &UserConfig) -> Result<ProjectDir, ConfigError> {
        let dirname = sanitize(cwd);
        let project_path = self.path.join(&dirname);
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

#[derive(Debug, Clone)]
pub struct ProjectDir {
    path: PathBuf,
}

impl ProjectDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state_path(&self) -> PathBuf {
        self.path.join("state.toml")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.path.join("sessions")
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

    pub fn session(&self, id: &str) -> SessionDir {
        SessionDir {
            path: self.sessions_dir().join(id),
        }
    }

    pub fn create_session(&self, id: &str) -> Result<SessionDir, ConfigError> {
        let dir = self.session(id);
        fs::create_dir_all(&dir.path)?;
        Ok(dir)
    }

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

#[derive(Debug, Clone)]
pub struct SessionDir {
    path: PathBuf,
}

impl SessionDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn history_path(&self) -> PathBuf {
        self.path.join("history.jsonl")
    }

    pub fn subagent(&self, id: &str) -> SessionDir {
        SessionDir {
            path: self.path.join("subagents").join(id),
        }
    }

    pub fn create_subagent(&self, id: &str) -> Result<SessionDir, ConfigError> {
        let dir = self.subagent(id);
        fs::create_dir_all(&dir.path)?;
        Ok(dir)
    }

    fn write_history_line(file: &mut File, msg: &Message) -> Result<(), ConfigError> {
        // 拼好 `line + \n` 后一次性 `write_all`,保证 1 次 `write(2)` syscall,
        // 避免 `file.write_all(line.as_bytes())?; file.write_all(b"\n")?;`
        // 那种两段写法带来的 2 次 syscall 退化。
        let mut bytes = serde_json::to_string(msg)?.into_bytes();
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        Ok(())
    }

    pub fn append_history(&self, msg: &Message) -> Result<(), ConfigError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.history_path())?;
        Self::write_history_line(&mut file, msg)?;
        // 强制 OS 把 page cache 落盘,避免进程在 panic/SIGKILL/断电 时丢末尾半行。
        file.sync_all()?;
        Ok(())
    }

    pub fn rewrite_history(&self, messages: &[Message]) -> Result<(), ConfigError> {
        fs::create_dir_all(&self.path)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.history_path())?;
        for msg in messages {
            Self::write_history_line(&mut file, msg)?;
        }
        // 整文件覆盖,循环结束一次性 sync;崩溃时最多丢未 sync 的最后几行。
        file.sync_all()?;
        Ok(())
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use omini_domain::message::{Message, Role, TextBlock};

    /// 临时目录,Drop 时清理。避免引入 `tempfile` 依赖。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("omini-config-test-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn session_dir(&self) -> SessionDir {
            let path = self.path.join("sessions").join("s1");
            fs::create_dir_all(&path).expect("create session dir");
            SessionDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![omini_domain::message::ContentBlock::Text(TextBlock {
                text: text.to_string(),
            })],
        }
    }

    #[test]
    fn append_history_writes_bytes_to_disk() {
        let tmp = TempDir::new();
        let dir = tmp.session_dir();
        let m1 = user_message("hello");
        dir.append_history(&m1).expect("first append");

        let m1_bytes = serde_json::to_string(&m1).unwrap();
        let raw = fs::read(dir.history_path()).expect("read history file");
        assert_eq!(raw, format!("{m1_bytes}\n").into_bytes());

        let m2 = user_message("world");
        dir.append_history(&m2).expect("second append");
        let m2_bytes = serde_json::to_string(&m2).unwrap();
        let raw = fs::read(dir.history_path()).expect("read history file");
        assert_eq!(raw, format!("{m1_bytes}\n{m2_bytes}\n").into_bytes());
    }

    #[test]
    fn rewrite_history_writes_full_file() {
        let tmp = TempDir::new();
        let dir = tmp.session_dir();
        let m1 = user_message("stale");
        dir.append_history(&m1).expect("seed");

        let m2 = user_message("two");
        let m3 = user_message("three");
        dir.rewrite_history(&[m2.clone(), m3.clone()])
            .expect("rewrite");
        let m2_bytes = serde_json::to_string(&m2).unwrap();
        let m3_bytes = serde_json::to_string(&m3).unwrap();
        let raw = fs::read(dir.history_path()).expect("read history file");
        assert_eq!(raw, format!("{m2_bytes}\n{m3_bytes}\n").into_bytes());
        assert!(!raw.windows(4).any(|w| w == b"stale"));

        dir.rewrite_history(&[]).expect("empty rewrite");
        let raw = fs::read(dir.history_path()).expect("read history file");
        assert!(
            raw.is_empty(),
            "empty rewrite should leave 0 bytes, got {raw:?}"
        );
    }
}
