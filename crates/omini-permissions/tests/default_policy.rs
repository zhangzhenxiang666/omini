use omini_domain::events::{
    ActiveProfile, BashPermissionPreview, PermissionPreview, ReadPermissionPreview,
    SearchPermissionPreview,
};
use omini_permissions::{PermissionCheck, PermissionDecision, PermissionEngine};
use serde_json::json;

#[test]
fn builtin_tools_without_configuration_apply_the_complete_default_matrix() {
    let engine = PermissionEngine::empty("/workspace");
    let cases = [
        (
            "read",
            Some(read_preview("/workspace/src/lib.rs")),
            PermissionDecision::Allow,
        ),
        (
            "view_image",
            Some(read_preview("/tmp/diagram.png")),
            PermissionDecision::Allow,
        ),
        ("search", None, PermissionDecision::Allow),
        ("edit", None, PermissionDecision::Ask),
        ("write", None, PermissionDecision::Ask),
        ("todo_write", None, PermissionDecision::Allow),
        ("ask_user", None, PermissionDecision::Allow),
        ("skill", None, PermissionDecision::Allow),
        ("spawn_agent", None, PermissionDecision::Allow),
        ("run_agent", None, PermissionDecision::Allow),
        ("get_task", None, PermissionDecision::Allow),
        ("cancel_task", None, PermissionDecision::Allow),
        ("bash", None, PermissionDecision::Ask),
        ("mcp__server__tool", None, PermissionDecision::Ask),
    ];

    for (tool, preview, decision) in cases {
        assert_eq!(
            engine.check(tool, preview.as_ref(), &json!({})),
            PermissionCheck {
                decision,
                source: None,
            },
            "unexpected default policy for {tool}"
        );
    }
}

#[test]
fn read_path_location_and_sensitivity_determine_the_default_decision() {
    let engine = PermissionEngine::empty("/workspace");
    let cases = [
        ("/workspace/src/lib.rs", PermissionDecision::Allow),
        ("/tmp/omini.log", PermissionDecision::Allow),
        ("/workspace/.env", PermissionDecision::Ask),
        ("/workspace/.env.production", PermissionDecision::Ask),
        ("/workspace/.ssh/config", PermissionDecision::Ask),
        ("/workspace/private.pem", PermissionDecision::Ask),
        ("/workspace/id_ed25519", PermissionDecision::Ask),
        ("/workspace/access_token.json", PermissionDecision::Ask),
        ("/etc/hosts", PermissionDecision::Ask),
        ("/workspace-other/file.rs", PermissionDecision::Ask),
        ("/workspace/../outside/file.rs", PermissionDecision::Ask),
        ("/tmp/../etc/passwd", PermissionDecision::Ask),
        ("", PermissionDecision::Ask),
    ];

    for (path, decision) in cases {
        assert_eq!(
            engine.check("read", Some(&read_preview(path)), &json!({})),
            PermissionCheck {
                decision,
                source: None,
            },
            "unexpected read policy for {path:?}"
        );
    }

    assert_eq!(
        engine.check("read", None, &json!({})),
        PermissionCheck {
            decision: PermissionDecision::Ask,
            source: None,
        }
    );
}

#[test]
fn search_path_missing_workspace_tmp_private_and_external_cases_are_distinct() {
    let engine = PermissionEngine::empty("/workspace");
    let cases = [
        (None, PermissionDecision::Allow),
        (Some("."), PermissionDecision::Allow),
        (Some("src"), PermissionDecision::Allow),
        (Some("/workspace/src"), PermissionDecision::Allow),
        (Some("/tmp/cache"), PermissionDecision::Allow),
        (Some("/workspace/.env"), PermissionDecision::Ask),
        (Some("/home/user/project"), PermissionDecision::Ask),
        (Some("/workspace/../outside"), PermissionDecision::Ask),
    ];

    for (path, decision) in cases {
        let preview = path.map(search_preview);
        assert_eq!(
            engine.check("search", preview.as_ref(), &json!({"query": "needle"})),
            PermissionCheck {
                decision,
                source: None,
            },
            "unexpected search policy for {path:?}"
        );
    }
}

#[test]
fn active_profile_matrix_only_hard_denies_mutating_tools_in_plan() {
    let engine = PermissionEngine::empty("/workspace");

    for profile in [ActiveProfile::Main, ActiveProfile::Auto] {
        for tool in ["edit", "write"] {
            assert_eq!(
                engine.check_for_profile(profile, tool, None, &json!({})),
                PermissionCheck {
                    decision: PermissionDecision::Ask,
                    source: None,
                }
            );
        }
        assert_eq!(
            engine.check_for_profile(profile, "todo_write", None, &json!({})),
            PermissionCheck {
                decision: PermissionDecision::Allow,
                source: None,
            }
        );
    }

    for tool in ["edit", "write", "todo_write"] {
        assert_eq!(
            engine.check_for_profile(ActiveProfile::Plan, tool, None, &json!({})),
            PermissionCheck {
                decision: PermissionDecision::Deny {
                    reason: format!("{tool} is not available in plan profile"),
                },
                source: None,
            }
        );
    }

    assert_eq!(
        engine.check_for_profile(ActiveProfile::Plan, "spawn_agent", None, &json!({})),
        PermissionCheck {
            decision: PermissionDecision::Allow,
            source: None,
        }
    );
}

#[test]
fn bash_missing_or_wrong_preview_falls_back_to_an_unattributed_prompt() {
    let engine = PermissionEngine::empty("/workspace");

    for preview in [None, Some(read_preview("/workspace/src/lib.rs"))] {
        assert_eq!(
            engine.check("bash", preview.as_ref(), &json!({"command": "cargo test"})),
            PermissionCheck {
                decision: PermissionDecision::Ask,
                source: None,
            }
        );
    }
}

#[test]
fn decision_helpers_return_the_decision_from_the_corresponding_check() {
    let engine = PermissionEngine::empty("/workspace");
    let preview = PermissionPreview::Bash(BashPermissionPreview {
        command: "cargo test".to_string(),
        description: None,
        workdir: None,
        timeout: 120_000,
    });

    assert_eq!(
        engine.decide("bash", Some(&preview), &json!({})),
        engine.check("bash", Some(&preview), &json!({})).decision
    );
    assert_eq!(
        engine.decide_for_profile(ActiveProfile::Auto, "bash", Some(&preview), &json!({})),
        engine
            .check_for_profile(ActiveProfile::Auto, "bash", Some(&preview), &json!({}))
            .decision
    );
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
