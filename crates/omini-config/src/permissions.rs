//! 权限配置"来源层"。
//!
//! 这个模块只负责读取与权限相关的配置文件,然后把原始内容交给
//! `omini-core` 的 [`PermissionEngine`] 去解析规则 DSL 并做 allow/ask/deny
//! 决策。它**不**解析 `prefix_rule(...)` / `Read(**/...)` 这类规则语法,
//! 也不参与 bash 内建风险策略 — 那些都是 `PermissionEngine` 的领域知识。
//!
//! 之所以把规则 DSL 解析留在 `omini-core` 而不是搬进 `omini-config`,
//! 是为了避免 `omini-config` 越界变成"通用规则引擎":
//!
//! 1. `omini-config` 应当只承担 schema / 路径 / 合并 / 校验等配置管理职责,
//!    不引入权限 DSL 词汇。
//! 2. 未来若独立 `omini-permissions` crate,只需挪动 `omini-core` 的部分,
//!    `load_permission_sources` 这层接口可以保持稳定。
//! 3. `PermissionEngine` 的 `matches()` 与 `CompiledPermissions` / `BashRule` /
//!    `ToolRule` 紧耦合,跨 crate 拆分会引入大量 DTO 噪音;一旦跨 crate,
//!    这些类型就要 pub 暴露字段,失去封装意义。
//!
//! [`PermissionEngine`]: ../../../omini_core/permissions/struct.PermissionEngine.html

use crate::settings::RawPermissionConfig;
use std::path::{Path, PathBuf};

/// 单个 `.rules` 文件的原始内容,供 `PermissionEngine` 进一步解析。
#[derive(Debug, Clone)]
pub struct RawBashRulesFile {
    pub path: PathBuf,
    pub content: String,
}

/// 已经加载好的权限配置来源集合。
///
/// `user_raw` 是 `~/.omini/config.toml [permissions]` 与
/// `<cwd>/.omini/config.toml [permissions]` 在 `UserConfig::merge_project_config`
/// 阶段的合并结果;`project_raw` 是 `<cwd>/.omini/permissions.toml`
/// (若存在)。两者作为**独立来源**交给 `PermissionEngine`,由 engine 内部的
/// stricter check 决定最终决策 — 不在来源层做字段级合并,以保持职责最小。
#[derive(Debug, Clone, Default)]
pub struct PermissionSources {
    /// 用户/项目 TOML `[permissions]` 的合并结果,带原始来源路径用于诊断。
    pub user_raw: Option<(RawPermissionConfig, PathBuf)>,
    /// `<cwd>/.omini/permissions.toml` 项目权限配置(若存在)。
    pub project_raw: Option<(RawPermissionConfig, PathBuf)>,
    /// `~/.omini/rules/*.rules` 与 `<cwd>/.omini/rules/*.rules` 的原始内容。
    pub bash_rule_files: Vec<RawBashRulesFile>,
    /// 文件 I/O / 解析阶段产生的诊断(例如读盘失败、规则被忽略)。
    pub diagnostics: Vec<String>,
}

impl PermissionSources {
    /// 构造一份只含 `user_raw` 的来源集合,供 core 单元测试使用,不做任何文件 I/O。
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
