use std::path::PathBuf;

use omini_config::{PermissionSources, RawBashRulesFile, RawPermissionConfig};
use omini_domain::events::{
    EditPermissionPreview, PermissionPreview, PermissionSource, ReadPermissionPreview,
    SearchPermissionPreview,
};
use omini_permissions::{PermissionCheck, PermissionDecision, PermissionEngine};
use serde_json::json;

const USER_SOURCE: &str = "/home/test/.omini/config.toml";
const PROJECT_SOURCE: &str = "/workspace/.omini/permissions.toml";

#[test]
fn matching_allow_ask_and_deny_rules_choose_the_strictest_decision_and_source() {
    let rule = "Read(**/*.rs)";
    let engine = engine_with_sources(
        raw(&[rule], &[rule], &[rule]),
        None,
        Some(PathBuf::from("/home/test")),
    );

    assert_eq!(
        engine.check(
            "read",
            Some(&read_preview("/workspace/src/lib.rs")),
            &json!({}),
        ),
        PermissionCheck {
            decision: PermissionDecision::Deny {
                reason: format!("Permission denied by rule: {rule}"),
            },
            source: Some(permission_source("deny", USER_SOURCE, rule)),
        }
    );
}

#[test]
fn user_and_project_sources_keep_the_stricter_effective_origin() {
    let rule = "Read(**/*.rs)";
    let engine = engine_with_sources(
        raw(&[rule], &[], &[]),
        Some(raw(&[], &[rule], &[])),
        Some(PathBuf::from("/home/test")),
    );

    assert_eq!(
        engine.check(
            "read",
            Some(&read_preview("/workspace/src/main.rs")),
            &json!({}),
        ),
        PermissionCheck {
            decision: PermissionDecision::Ask,
            source: Some(permission_source("ask", PROJECT_SOURCE, rule)),
        }
    );
}

#[test]
fn supported_tool_names_and_agent_aliases_match_their_external_inputs() {
    let rules = [
        "Read(**/blocked.txt)",
        "Search(**/blocked)",
        "Edit(**/blocked.txt)",
        "Write(**/blocked.txt)",
        "Agent(explorer)",
        "AskUser",
        "TodoWrite",
    ];
    let engine = engine_with_sources(
        raw(&[], &[], &rules),
        None,
        Some(PathBuf::from("/home/test")),
    );
    let path = "/workspace/blocked.txt";
    let cases = [
        ("read", Some(read_preview(path)), json!({}), rules[0]),
        (
            "view_image",
            Some(read_preview(path)),
            json!({"path": path}),
            rules[0],
        ),
        (
            "search",
            Some(search_preview("/workspace/blocked")),
            json!({}),
            rules[1],
        ),
        ("edit", Some(edit_preview(path, false)), json!({}), rules[2]),
        ("write", Some(edit_preview(path, true)), json!({}), rules[3]),
        ("spawn_agent", None, json!({"name": "explorer"}), rules[4]),
        ("run_agent", None, json!({"name": "explorer"}), rules[4]),
        ("ask_user", None, json!({}), rules[5]),
        ("todo_write", None, json!({}), rules[6]),
    ];

    for (tool, preview, input, rule) in cases {
        assert_eq!(
            engine.check(tool, preview.as_ref(), &input),
            PermissionCheck {
                decision: PermissionDecision::Deny {
                    reason: format!("Permission denied by rule: {rule}"),
                },
                source: Some(permission_source("deny", USER_SOURCE, rule)),
            },
            "rule {rule} should apply to {tool}"
        );
    }

    assert_eq!(
        engine.check("spawn_agent", None, &json!({"name": "reviewer"})),
        PermissionCheck {
            decision: PermissionDecision::Allow,
            source: None,
        }
    );
}

#[test]
fn path_specifiers_resolve_against_project_home_and_filesystem_root() {
    let engine = engine_with_sources(
        raw(
            &["Read(/etc/**)"],
            &[],
            &["Read(./src/private/**)", "Read(~/secrets/**)"],
        ),
        None,
        Some(PathBuf::from("/home/test")),
    );
    let cases = [
        (
            "/etc/hosts",
            PermissionCheck {
                decision: PermissionDecision::Allow,
                source: Some(permission_source("allow", USER_SOURCE, "Read(/etc/**)")),
            },
        ),
        (
            "/workspace/src/private/notes.txt",
            denied("Read(./src/private/**)"),
        ),
        ("/home/test/secrets/notes.txt", denied("Read(~/secrets/**)")),
        (
            "/workspace/src/private/../../public.txt",
            PermissionCheck {
                decision: PermissionDecision::Allow,
                source: None,
            },
        ),
    ];

    for (path, expected) in cases {
        assert_eq!(
            engine.check("read", Some(&read_preview(path)), &json!({})),
            expected,
            "unexpected path rule result for {path}"
        );
    }
}

#[test]
fn recursive_wildcard_matches_both_direct_and_nested_descendants() {
    let rule = "Read(**/.env)";
    let engine = engine_with_sources(
        raw(&[], &[], &[rule]),
        None,
        Some(PathBuf::from("/home/test")),
    );

    for path in ["/workspace/.env", "/workspace/crates/app/.env"] {
        assert_eq!(
            engine.check("read", Some(&read_preview(path)), &json!({})),
            denied(rule),
            "recursive wildcard should match {path}"
        );
    }
}

#[test]
fn path_scoped_rule_without_a_path_does_not_activate() {
    let engine = engine_with_sources(
        raw(&[], &[], &["Read(**/*.rs)"]),
        None,
        Some(PathBuf::from("/home/test")),
    );

    assert_eq!(
        engine.check("read", None, &json!({})),
        PermissionCheck {
            decision: PermissionDecision::Ask,
            source: None,
        }
    );
}

#[test]
fn unsupported_or_malformed_inline_rules_emit_complete_ordered_diagnostics() {
    let sources = PermissionSources {
        user_raw: Some((
            RawPermissionConfig {
                allow: vec![
                    "Fetch(*)".to_string(),
                    "Bash(cargo test)".to_string(),
                    "Read(**/*.toml".to_string(),
                    "Read()".to_string(),
                    "   ".to_string(),
                ],
                ask: Vec::new(),
                deny: Vec::new(),
            },
            PathBuf::from(PROJECT_SOURCE),
        )),
        ..PermissionSources::default()
    };

    let engine =
        PermissionEngine::from_sources("/workspace", Some(PathBuf::from("/home/test")), sources);

    assert_eq!(
        engine.diagnostics(),
        [
            format!(
                "{PROJECT_SOURCE}: ignored permission rule 'Fetch(*)': unsupported tool 'Fetch'"
            ),
            format!(
                "{PROJECT_SOURCE}: ignored permission rule 'Bash(cargo test)': Bash rules must be configured in .omini/rules/*.rules"
            ),
            format!(
                "{PROJECT_SOURCE}: ignored permission rule 'Read(**/*.toml': invalid permission rule syntax"
            ),
            format!(
                "{PROJECT_SOURCE}: ignored permission rule 'Read()': empty permission rule specifier"
            ),
        ]
    );
}

#[test]
fn source_diagnostics_and_bad_bash_rules_are_preserved_while_later_rules_still_apply() {
    let rules_path = "/workspace/.omini/rules/project.rules";
    let mut sources = PermissionSources {
        diagnostics: vec!["permission source loading warning".to_string()],
        ..PermissionSources::default()
    };
    sources.user_raw = Some((raw(&["Fetch(*)"], &[], &[]), PathBuf::from(USER_SOURCE)));
    sources.bash_rule_files.push(RawBashRulesFile {
        path: PathBuf::from(rules_path),
        content: r#"
prefix_rule(pattern = ["broken"], decision = "later")
prefix_rule(pattern = ["custom-safe"], decision = "allow")
"#
        .to_string(),
    });

    let engine =
        PermissionEngine::from_sources("/workspace", Some(PathBuf::from("/home/test")), sources);

    assert_eq!(
        engine.diagnostics(),
        [
            "permission source loading warning".to_string(),
            format!("{USER_SOURCE}: ignored permission rule 'Fetch(*)': unsupported tool 'Fetch'"),
            format!("{rules_path}: 跳过 prefix_rule #1: invalid decision 'later'"),
        ]
    );
    assert_eq!(
        engine.check(
            "bash",
            Some(&bash_preview("custom-safe --verbose")),
            &json!({}),
        ),
        PermissionCheck {
            decision: PermissionDecision::Allow,
            source: Some(PermissionSource {
                decision: "allow".to_string(),
                source: rules_path.to_string(),
                rule: "prefix_rule #2".to_string(),
            }),
        }
    );
}

fn raw(allow: &[&str], ask: &[&str], deny: &[&str]) -> RawPermissionConfig {
    RawPermissionConfig {
        allow: strings(allow),
        ask: strings(ask),
        deny: strings(deny),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn engine_with_sources(
    user: RawPermissionConfig,
    project: Option<RawPermissionConfig>,
    home: Option<PathBuf>,
) -> PermissionEngine {
    PermissionEngine::from_sources(
        "/workspace",
        home,
        PermissionSources {
            user_raw: Some((user, PathBuf::from(USER_SOURCE))),
            project_raw: project.map(|raw| (raw, PathBuf::from(PROJECT_SOURCE))),
            ..PermissionSources::default()
        },
    )
}

fn permission_source(decision: &str, source: &str, rule: &str) -> PermissionSource {
    PermissionSource {
        decision: decision.to_string(),
        source: source.to_string(),
        rule: rule.to_string(),
    }
}

fn denied(rule: &str) -> PermissionCheck {
    PermissionCheck {
        decision: PermissionDecision::Deny {
            reason: format!("Permission denied by rule: {rule}"),
        },
        source: Some(permission_source("deny", USER_SOURCE, rule)),
    }
}

fn read_preview(path: &str) -> PermissionPreview {
    PermissionPreview::Read(ReadPermissionPreview {
        file_path: path.to_string(),
    })
}

fn search_preview(path: &str) -> PermissionPreview {
    PermissionPreview::Search(SearchPermissionPreview {
        query: "needle".to_string(),
        mode: "content".to_string(),
        path: path.to_string(),
    })
}

fn edit_preview(path: &str, write: bool) -> PermissionPreview {
    let preview = EditPermissionPreview {
        summary: "change file".to_string(),
        path: path.to_string(),
        replacement_count: 1,
        diff: "diff".to_string(),
    };
    if write {
        PermissionPreview::Write(preview)
    } else {
        PermissionPreview::Edit(preview)
    }
}

fn bash_preview(command: &str) -> PermissionPreview {
    PermissionPreview::Bash(omini_domain::events::BashPermissionPreview {
        command: command.to_string(),
        description: None,
        workdir: None,
        timeout: 120_000,
    })
}
