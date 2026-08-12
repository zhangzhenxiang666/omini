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

    for dir in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rules"))
            .collect();
        paths.sort();
        for path in paths {
            match std::fs::read_to_string(&path) {
                Ok(content) => files.push(RawBashRulesFile { path, content }),
                Err(e) => diagnostics.push(format!(
                    "{}: failed to read rules file: {e}",
                    path.display()
                )),
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_root(label: &str) -> PathBuf {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "omini-config-permissions-{label}-{}-{nanos}-{seq}",
            std::process::id(),
        ))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn empty_inputs_yield_empty_sources_without_diagnostics() {
        let cwd = unique_root("empty-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let home = unique_root("empty-home");
        fs::create_dir_all(&home).unwrap();

        let sources = load_permission_sources(&cwd, Some(&home), None);

        assert!(sources.user_raw.is_none());
        assert!(sources.project_raw.is_none());
        assert!(sources.bash_rule_files.is_empty());
        assert!(sources.diagnostics().is_empty());

        cleanup(&cwd);
        cleanup(&home);
    }

    #[test]
    fn missing_home_dir_does_not_emit_diagnostics() {
        let cwd = unique_root("no-home-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let home = unique_root("no-home-home");

        let sources = load_permission_sources(&cwd, Some(&home), None);

        assert!(sources.user_raw.is_none());
        assert!(sources.bash_rule_files.is_empty());
        assert!(sources.diagnostics().is_empty());

        cleanup(&cwd);
    }

    #[test]
    fn user_only_permissions_toml_loads_into_project_raw() {
        let cwd = unique_root("user-only-cwd");
        fs::create_dir_all(cwd.join(".omini")).unwrap();
        fs::write(
            cwd.join(".omini").join("permissions.toml"),
            r#"
allow = ["Read"]
deny = ["Write"]
"#,
        )
        .unwrap();
        let home = unique_root("user-only-home");

        let sources = load_permission_sources(&cwd, Some(&home), None);

        let (raw, path) = sources
            .project_raw
            .as_ref()
            .expect("project permissions source should be present");
        assert!(path.ends_with(".omini/permissions.toml"));
        assert_eq!(raw.allow, vec!["Read".to_string()]);
        assert_eq!(raw.deny, vec!["Write".to_string()]);
        assert!(
            sources.user_raw.is_none(),
            "user_raw should be None when caller passed no user config [permissions]"
        );
        assert!(sources.diagnostics().is_empty());

        cleanup(&cwd);
    }

    #[test]
    fn user_config_permissions_only_keeps_user_raw_separate() {
        let cwd = unique_root("user-only-config-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let home = unique_root("user-only-config-home");

        let user = RawPermissionConfig {
            allow: vec!["UserRead".to_string()],
            ask: Vec::new(),
            deny: Vec::new(),
        };
        let sources = load_permission_sources(&cwd, Some(&home), Some(user));

        let (merged, _) = sources
            .user_raw
            .as_ref()
            .expect("user_raw should be populated from caller");
        assert_eq!(merged.allow, vec!["UserRead".to_string()]);
        assert!(sources.project_raw.is_none());
        assert!(sources.diagnostics().is_empty());

        cleanup(&cwd);
    }

    #[test]
    fn user_and_project_permissions_both_present_emit_combined_diagnostic() {
        let cwd = unique_root("both-cwd");
        fs::create_dir_all(cwd.join(".omini")).unwrap();
        fs::write(
            cwd.join(".omini").join("permissions.toml"),
            r#"
allow = ["ProjectRead"]
deny = ["ProjectWrite"]
"#,
        )
        .unwrap();
        let home = unique_root("both-home");

        let user = RawPermissionConfig {
            allow: vec!["UserRead".to_string()],
            ask: vec!["UserAsk".to_string()],
            deny: Vec::new(),
        };
        let sources = load_permission_sources(&cwd, Some(&home), Some(user));

        // 两侧都按独立来源保留,不做字段级合并
        let (user_merged, _) = sources
            .user_raw
            .as_ref()
            .expect("user_raw should be present");
        assert_eq!(user_merged.allow, vec!["UserRead".to_string()]);
        let (project, _) = sources
            .project_raw
            .as_ref()
            .expect("project_raw should be present");
        assert_eq!(project.allow, vec!["ProjectRead".to_string()]);
        assert_eq!(project.deny, vec!["ProjectWrite".to_string()]);
        let diagnostics = sources.diagnostics();
        assert!(
            diagnostics.iter().any(|d| d.contains("both present")),
            "expected 'both present' diagnostic, got: {diagnostics:?}"
        );

        cleanup(&cwd);
    }

    #[test]
    fn home_and_project_rule_files_are_loaded_and_sorted() {
        let cwd = unique_root("rules-cwd");
        let home = unique_root("rules-home");
        fs::create_dir_all(cwd.join(".omini").join("rules")).unwrap();
        fs::create_dir_all(home.join(".omini").join("rules")).unwrap();

        fs::write(
            cwd.join(".omini").join("rules").join("project_b.rules"),
            "project_b content",
        )
        .unwrap();
        fs::write(
            cwd.join(".omini").join("rules").join("project_a.rules"),
            "project_a content",
        )
        .unwrap();
        fs::write(
            home.join(".omini").join("rules").join("home.rules"),
            "home content",
        )
        .unwrap();
        // 非 .rules 后缀文件应被忽略
        fs::write(
            cwd.join(".omini").join("rules").join("ignored.txt"),
            "ignored",
        )
        .unwrap();

        let sources = load_permission_sources(&cwd, Some(&home), None);

        let names: Vec<String> = sources
            .bash_rule_files
            .iter()
            .map(|f| {
                f.path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            names,
            vec!["home.rules", "project_a.rules", "project_b.rules"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
        assert!(sources.diagnostics().is_empty());

        cleanup(&cwd);
        cleanup(&home);
    }

    #[test]
    fn missing_rules_directories_do_not_emit_diagnostics() {
        let cwd = unique_root("no-rules-cwd");
        fs::create_dir_all(&cwd).unwrap();
        let home = unique_root("no-rules-home");
        fs::create_dir_all(&home).unwrap();

        let sources = load_permission_sources(&cwd, Some(&home), None);

        assert!(sources.bash_rule_files.is_empty());
        assert!(sources.diagnostics().is_empty());

        cleanup(&cwd);
        cleanup(&home);
    }

    #[test]
    fn malformed_permissions_toml_emits_diagnostic_without_aborting() {
        let cwd = unique_root("malformed-cwd");
        fs::create_dir_all(cwd.join(".omini")).unwrap();
        fs::write(
            cwd.join(".omini").join("permissions.toml"),
            "this is not valid toml = = =",
        )
        .unwrap();
        let home = unique_root("malformed-home");

        let sources = load_permission_sources(&cwd, Some(&home), None);

        assert!(sources.project_raw.is_none());
        assert!(
            sources
                .diagnostics()
                .iter()
                .any(|d| d.contains("failed to parse permissions file")),
            "expected parse diagnostic, got: {:?}",
            sources.diagnostics()
        );

        cleanup(&cwd);
    }

    #[test]
    fn from_raw_seeds_user_raw_only() {
        let raw = RawPermissionConfig {
            allow: vec!["Read".to_string()],
            ask: Vec::new(),
            deny: Vec::new(),
        };
        let sources = PermissionSources::from_raw(raw);

        let (stored, path) = sources.user_raw.as_ref().expect("user_raw seeded");
        assert_eq!(stored.allow, vec!["Read".to_string()]);
        assert_eq!(*path, PathBuf::from("<inline>"));
        assert!(sources.project_raw.is_none());
        assert!(sources.bash_rule_files.is_empty());
        assert!(sources.diagnostics().is_empty());
    }
}
