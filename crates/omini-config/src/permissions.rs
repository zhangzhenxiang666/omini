use crate::settings::RawPermissionConfig;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RawBashRulesFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct PermissionSources {
    pub user_raw: Option<(RawPermissionConfig, PathBuf)>,
    pub project_raw: Option<(RawPermissionConfig, PathBuf)>,
    pub bash_rule_files: Vec<RawBashRulesFile>,
    pub diagnostics: Vec<String>,
}

impl PermissionSources {
    pub fn from_raw(raw: RawPermissionConfig) -> Self {
        Self {
            user_raw: Some((raw, PathBuf::from("<inline>"))),
            project_raw: None,
            bash_rule_files: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

pub fn load_permission_sources(
    cwd: &Path,
    home: Option<&Path>,
    user_raw: Option<RawPermissionConfig>,
) -> PermissionSources {
    let mut diagnostics = Vec::new();

    let project_raw = read_project_permissions_file(cwd, &mut diagnostics);
    let bash_rule_files = scan_bash_rule_files(cwd, home, &mut diagnostics);

    if user_raw.is_some() && project_raw.is_some() {
        diagnostics.push(
            "user/project config [permissions] and <cwd>/.omini/permissions.toml both present; \
             rules from both sources are loaded and the stricter decision wins"
                .to_string(),
        );
    }

    PermissionSources {
        user_raw: user_raw.map(|raw| (raw, user_config_path(home))),
        project_raw,
        bash_rule_files,
        diagnostics,
    }
}

fn read_project_permissions_file(
    cwd: &Path,
    diagnostics: &mut Vec<String>,
) -> Option<(RawPermissionConfig, PathBuf)> {
    let path = cwd.join(".omini").join("permissions.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            diagnostics.push(format!(
                "{}: failed to read permissions file: {e}",
                path.display()
            ));
            return None;
        }
    };
    match toml::from_str(&content) {
        Ok(raw) => Some((raw, path)),
        Err(e) => {
            diagnostics.push(format!(
                "{}: failed to parse permissions file: {e}",
                path.display()
            ));
            None
        }
    }
}

fn scan_bash_rule_files(
    cwd: &Path,
    home: Option<&Path>,
    diagnostics: &mut Vec<String>,
) -> Vec<RawBashRulesFile> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".omini").join("rules"));
    }
    dirs.push(cwd.join(".omini").join("rules"));

    let mut paths = Vec::new();
    for dir in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        paths.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "rules")),
        );
    }

    // 用户级与项目级规则共同按文件名排序；同名时稳定保留用户级在前。
    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    for path in paths {
        match std::fs::read_to_string(&path) {
            Ok(content) => files.push(RawBashRulesFile { path, content }),
            Err(e) => diagnostics.push(format!(
                "{}: failed to read rules file: {e}",
                path.display()
            )),
        }
    }
    files
}

fn user_config_path(home: Option<&Path>) -> PathBuf {
    home.map(Path::to_path_buf)
        .or_else(dirs::home_dir)
        .map(|home| home.join(".omini").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("~/.omini/config.toml"))
}
