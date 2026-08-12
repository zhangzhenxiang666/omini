use omini_domain::subagents::{AgentDraft, AgentSourceKind};
use omini_domain::title_generation::GeneratedThreadTitle;
use omini_domain::tool::ToolDefinition;
use omini_domain::usage::Usage;
use serde_json::json;

#[test]
fn agent_source_kind_all_variants_share_label_and_json_contracts() {
    for (kind, json_name, label) in [
        (AgentSourceKind::BuiltIn, "built_in", "内置"),
        (AgentSourceKind::Project, "project", "项目"),
        (AgentSourceKind::User, "user", "用户"),
    ] {
        assert_eq!(kind.label(), label);
        assert_eq!(
            serde_json::to_value(kind).expect("source kind should serialize"),
            json!(json_name)
        );
        assert_eq!(
            serde_json::from_value::<AgentSourceKind>(json!(json_name))
                .expect("source kind should deserialize"),
            kind
        );
    }
}

#[test]
fn agent_draft_empty_optional_collections_and_values_are_omitted() {
    let draft = AgentDraft {
        name: "reviewer".into(),
        description: "Reviews code".into(),
        short_description: None,
        instructions: "Review carefully".into(),
        tools: Vec::new(),
        disallow_tools: Vec::new(),
        model: None,
    };

    assert_eq!(
        serde_json::to_value(draft).expect("agent draft should serialize"),
        json!({
            "name": "reviewer",
            "description": "Reviews code",
            "instructions": "Review carefully"
        })
    );
}

#[test]
fn agent_draft_nonempty_optional_values_are_preserved() {
    let value = json!({
        "name": "reviewer",
        "description": "Reviews code",
        "short_description": "Review",
        "instructions": "Review carefully",
        "tools": ["read", "search"],
        "disallow_tools": ["write"],
        "model": "large"
    });

    let draft: AgentDraft =
        serde_json::from_value(value.clone()).expect("agent draft should deserialize");
    assert_eq!(
        serde_json::to_value(draft).expect("agent draft should serialize"),
        value
    );
}

#[test]
fn generated_title_and_tool_definition_keep_their_public_json_shapes() {
    assert_eq!(
        serde_json::to_value(GeneratedThreadTitle {
            title: "A concise title".into(),
        })
        .expect("generated title should serialize"),
        json!({"title": "A concise title"})
    );
    assert_eq!(
        serde_json::to_value(ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        })
        .expect("tool definition should serialize"),
        json!({
            "name": "read",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        })
    );
}

#[test]
fn usage_total_counts_prompt_and_completion_but_not_cached_tokens() {
    assert_eq!(Usage::default().total_tokens(), 0);
    assert_eq!(
        Usage {
            prompt_tokens: 7,
            completion_tokens: 5,
            cached_tokens: usize::MAX,
        }
        .total_tokens(),
        12
    );
    assert_eq!(
        Usage {
            prompt_tokens: usize::MAX - 1,
            completion_tokens: 1,
            cached_tokens: 0,
        }
        .total_tokens(),
        usize::MAX
    );
}
