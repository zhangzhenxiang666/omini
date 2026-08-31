mod support;

use omini_config::RawConfig;
use omini_core::{
    compacted_plan_context, delete_project_agent, project_agents_snapshot, save_project_agent,
};
use omini_domain::subagents::{AgentDraft, AgentSourceKind};
use omini_runtime_contract::project::{DeleteProjectAgentCommand, SaveProjectAgentCommand};

fn draft(name: &str) -> AgentDraft {
    AgentDraft {
        name: name.into(),
        description: "Use for an isolated integration-test task.".into(),
        short_description: Some("测试助手".into()),
        instructions: "Inspect the scoped input and report the result.".into(),
        tools: vec!["read".into()],
        disallow_tools: vec!["write".into()],
        model: None,
    }
}

#[test]
fn project_agent_facade_saves_lists_and_deletes_project_agent() {
    let temp = support::TestTempDir::new("project-agent");
    let name = format!("core-test-agent-{}", std::process::id());
    let command = SaveProjectAgentCommand {
        source_kind: AgentSourceKind::Project,
        original_agent_id: None,
        draft: draft(&name),
    };

    let saved = save_project_agent(temp.path(), command).expect("project agent should save");
    let record = saved
        .records
        .iter()
        .find(|record| record.name == name)
        .expect("saved agent should be returned");
    assert!(record.editable);
    assert_eq!(record.source_kind, AgentSourceKind::Project);
    let agent_id = record
        .path
        .as_ref()
        .expect("project agent should have a path")
        .display()
        .to_string();

    let deleted = delete_project_agent(temp.path(), DeleteProjectAgentCommand { agent_id })
        .expect("saved project agent should delete");
    assert!(deleted.records.iter().all(|record| record.name != name));
}

#[test]
fn project_agent_facade_rejects_built_in_writes_and_sorts_models() {
    let temp = support::TestTempDir::new("project-agent-reject");
    let error = save_project_agent(
        temp.path(),
        SaveProjectAgentCommand {
            source_kind: AgentSourceKind::BuiltIn,
            original_agent_id: None,
            draft: draft("ignored"),
        },
    )
    .expect_err("built-in agents must not be writable");
    assert_eq!(error.code(), "core_error");
    assert_eq!(error.message(), "内置 agent 不能写入");

    let raw: RawConfig = toml::from_str(
        r#"
[providers.z-provider]
protocol = "openai"
base_url = "http://127.0.0.1:9"

[providers.z-provider.models.test]

[providers.a-provider]
protocol = "openai"
base_url = "http://127.0.0.1:9"

[providers.a-provider.models.test]
"#,
    )
    .expect("test config should parse");
    let settings = raw
        .resolve()
        .expect("test config should resolve")
        .to_settings(Some("z-provider"), Some("test"), None, temp.path())
        .expect("test settings should build");
    let snapshot = project_agents_snapshot(&settings);
    assert_eq!(
        snapshot
            .providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-provider", "z-provider"]
    );
}

#[test]
fn compacted_plan_context_keeps_plan_between_required_execution_guidance() {
    let plan = "# Plan\n\n1. Add focused tests.";
    assert_eq!(
        compacted_plan_context(plan),
        "A previous planning pass produced the approved plan below to accomplish the user's task. Implement the plan in a fresh context. Treat the plan as the source of user intent, re-read files as needed, and carry the work through implementation and verification.\n\nApproved plan:\n# Plan\n\n1. Add focused tests.\n\nIntermediate planning discussion and discarded alternatives were intentionally omitted."
    );
}
