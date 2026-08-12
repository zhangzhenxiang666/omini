mod support;

use omini_config::{PermissionSources, RawPermissionConfig, load_permission_sources};
use std::path::PathBuf;
use support::TestTempDir;

fn raw_permissions(allow: &[&str], ask: &[&str], deny: &[&str]) -> RawPermissionConfig {
    RawPermissionConfig {
        allow: allow.iter().map(|value| (*value).to_string()).collect(),
        ask: ask.iter().map(|value| (*value).to_string()).collect(),
        deny: deny.iter().map(|value| (*value).to_string()).collect(),
    }
}

#[test]
fn empty_permission_sources_stay_empty() {
    let temp = TestTempDir::new("permissions-empty");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");

    let sources = load_permission_sources(&cwd, Some(&home), None);

    assert!(sources.user_raw.is_none());
    assert!(sources.project_raw.is_none());
    assert!(sources.bash_rule_files.is_empty());
    assert!(sources.diagnostics().is_empty());
}

#[test]
fn missing_permission_directories_are_silent() {
    let temp = TestTempDir::new("permissions-missing-dirs");
    let cwd = temp.create_dir("workspace");
    let missing_home = temp.path().join("missing-home");

    let sources = load_permission_sources(&cwd, Some(&missing_home), None);

    assert!(sources.user_raw.is_none());
    assert!(sources.project_raw.is_none());
    assert!(sources.bash_rule_files.is_empty());
    assert!(sources.diagnostics().is_empty());
}

#[test]
fn inline_permissions_keep_user_origin() {
    let temp = TestTempDir::new("permissions-inline");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");
    let raw = raw_permissions(&["Read"], &["Edit"], &["Write"]);

    let sources = load_permission_sources(&cwd, Some(&home), Some(raw));

    let (stored, path) = sources
        .user_raw
        .as_ref()
        .expect("user source should be retained");
    assert_eq!(stored.allow, ["Read"]);
    assert_eq!(stored.ask, ["Edit"]);
    assert_eq!(stored.deny, ["Write"]);
    assert_eq!(path, &home.join(".omini/config.toml"));
    assert!(sources.project_raw.is_none());
    assert!(sources.bash_rule_files.is_empty());
    assert!(sources.diagnostics().is_empty());
}

#[test]
fn project_permissions_load_defaults_and_path() {
    let temp = TestTempDir::new("permissions-project");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");
    let path = temp.write(
        "workspace/.omini/permissions.toml",
        r#"
allow = ["Read"]
deny = ["Write"]
"#,
    );

    let sources = load_permission_sources(&cwd, Some(&home), None);

    let (stored, stored_path) = sources
        .project_raw
        .as_ref()
        .expect("project permissions should load");
    assert_eq!(stored.allow, ["Read"]);
    assert!(stored.ask.is_empty());
    assert_eq!(stored.deny, ["Write"]);
    assert_eq!(stored_path, &path);
    assert!(sources.user_raw.is_none());
    assert!(sources.diagnostics().is_empty());
}

#[test]
fn competing_permission_sources_stay_distinct() {
    let temp = TestTempDir::new("permissions-both");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");
    let project_path = temp.write(
        "workspace/.omini/permissions.toml",
        r#"
allow = ["ProjectRead"]
ask = ["ProjectEdit"]
deny = ["ProjectWrite"]
"#,
    );

    let sources = load_permission_sources(
        &cwd,
        Some(&home),
        Some(raw_permissions(
            &["UserRead"],
            &["UserEdit"],
            &["UserWrite"],
        )),
    );

    // 两个 TOML 来源不在 config 层合并，权限引擎才能保留来源并执行更严格优先。
    let (user, user_path) = sources.user_raw.as_ref().expect("user source should exist");
    let (project, stored_project_path) = sources
        .project_raw
        .as_ref()
        .expect("project source should exist");
    assert_eq!(user.allow, ["UserRead"]);
    assert_eq!(user.ask, ["UserEdit"]);
    assert_eq!(user.deny, ["UserWrite"]);
    assert_eq!(user_path, &home.join(".omini/config.toml"));
    assert_eq!(project.allow, ["ProjectRead"]);
    assert_eq!(project.ask, ["ProjectEdit"]);
    assert_eq!(project.deny, ["ProjectWrite"]);
    assert_eq!(stored_project_path, &project_path);
    assert_eq!(
        sources.diagnostics(),
        [
            "user/project config [permissions] and <cwd>/.omini/permissions.toml both present; rules from both sources are loaded and the stricter decision wins"
        ]
    );
}

#[test]
fn malformed_permissions_report_parse_failure() {
    let temp = TestTempDir::new("permissions-malformed");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");
    let path = temp.write(
        "workspace/.omini/permissions.toml",
        "this is not valid toml = = =",
    );

    let sources = load_permission_sources(&cwd, Some(&home), None);

    assert!(sources.project_raw.is_none());
    assert!(sources.bash_rule_files.is_empty());
    assert_eq!(sources.diagnostics().len(), 1);
    let diagnostic = &sources.diagnostics()[0];
    assert!(diagnostic.starts_with(&path.display().to_string()));
    assert!(diagnostic.contains("failed to parse permissions file"));
}

#[test]
fn unreadable_permissions_report_io_failure() {
    let temp = TestTempDir::new("permissions-read-failure");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");
    let path = temp.create_dir("workspace/.omini/permissions.toml");

    let sources = load_permission_sources(&cwd, Some(&home), None);

    assert!(sources.project_raw.is_none());
    assert_eq!(sources.diagnostics().len(), 1);
    let diagnostic = &sources.diagnostics()[0];
    assert!(diagnostic.starts_with(&path.display().to_string()));
    assert!(diagnostic.contains("failed to read permissions file"));
}

#[test]
fn rule_files_share_one_global_filename_order() {
    let temp = TestTempDir::new("permissions-rule-order");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");
    let user_z = temp.write("home/.omini/rules/z-user.rules", "user z");
    let user_same = temp.write("home/.omini/rules/shared.rules", "user shared");
    let project_a = temp.write("workspace/.omini/rules/a-project.rules", "project a");
    let project_same = temp.write(
        "workspace/.omini/rules/shared.rules",
        "project shared\ninvalid DSL is preserved",
    );
    temp.write("workspace/.omini/rules/ignored.txt", "ignored");

    let sources = load_permission_sources(&cwd, Some(&home), None);

    // 同名文件稳定保持用户级在前；规则内容由 permissions crate 解析，这里只原样加载。
    let actual = sources
        .bash_rule_files
        .iter()
        .map(|file| (file.path.clone(), file.content.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (project_a, "project a"),
            (user_same, "user shared"),
            (project_same, "project shared\ninvalid DSL is preserved"),
            (user_z, "user z"),
        ]
    );
    assert!(sources.diagnostics().is_empty());
}

#[test]
fn unreadable_rules_do_not_block_good_files() {
    let temp = TestTempDir::new("permissions-rule-read-failure");
    let cwd = temp.create_dir("workspace");
    let home = temp.create_dir("home");
    let good = temp.write("workspace/.omini/rules/good.rules", "good content");
    let broken = temp.create_dir("workspace/.omini/rules/broken.rules");

    let sources = load_permission_sources(&cwd, Some(&home), None);

    assert_eq!(sources.bash_rule_files.len(), 1);
    assert_eq!(sources.bash_rule_files[0].path, good);
    assert_eq!(sources.bash_rule_files[0].content, "good content");
    assert_eq!(sources.diagnostics().len(), 1);
    let diagnostic = &sources.diagnostics()[0];
    assert!(diagnostic.starts_with(&broken.display().to_string()));
    assert!(diagnostic.contains("failed to read rules file"));
}

#[test]
fn from_raw_builds_inline_source() {
    let sources = PermissionSources::from_raw(raw_permissions(&["Read"], &[], &[]));

    let (stored, path) = sources
        .user_raw
        .as_ref()
        .expect("inline source should exist");
    assert_eq!(stored.allow, ["Read"]);
    assert!(stored.ask.is_empty());
    assert!(stored.deny.is_empty());
    assert_eq!(path, &PathBuf::from("<inline>"));
    assert!(sources.project_raw.is_none());
    assert!(sources.bash_rule_files.is_empty());
    assert!(sources.diagnostics().is_empty());
}
