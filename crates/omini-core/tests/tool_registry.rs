use omini_core::tools::{
    Tool, ToolResult,
    agent_tools::{RunAgentTool, SpawnAgentTool},
    create_agent_registry_from_parent, create_main_registry,
};
use serde_json::json;

#[test]
fn main_registry_exposes_ordered_tool_contracts() {
    let registry = create_main_registry();
    assert_eq!(
        registry.tool_names(),
        vec![
            "ask_user",
            "bash",
            "cancel_task",
            "edit",
            "get_task",
            "read",
            "search",
            "skill",
            "spawn_agent",
            "todo_write",
            "view_image",
            "write",
        ]
    );
    assert_eq!(
        registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>(),
        vec![
            "search",
            "read",
            "view_image",
            "edit",
            "write",
            "bash",
            "ask_user",
            "skill",
            "todo_write",
            "spawn_agent",
            "get_task",
            "cancel_task",
        ]
    );

    let todo = registry
        .definitions()
        .into_iter()
        .find(|definition| definition.name == "todo_write")
        .expect("todo_write definition");
    let todo_item = &todo.input_schema["properties"]["todos"]["items"]["properties"];
    assert!(todo_item["content"].is_object());
    assert!(todo_item["status"].is_object());
    assert!(todo_item.get("step").is_none());

    for schema in [SpawnAgentTool.input_schema(), RunAgentTool.input_schema()] {
        let required = schema["required"].as_array().expect("required fields");
        for field in ["name", "prompt", "title"] {
            assert!(required.iter().any(|value| value.as_str() == Some(field)));
        }
    }
    assert!(
        SpawnAgentTool
            .description()
            .contains("automatic notification")
    );
}

#[test]
fn agent_registry_applies_parent_allow_deny_and_depth_rules() {
    let parent = create_main_registry();
    let allow = vec![
        "read".to_string(),
        "search".to_string(),
        "write".to_string(),
        "run_agent".to_string(),
        "missing".to_string(),
    ];
    let deny = vec!["write".to_string()];
    let (child, warnings) = create_agent_registry_from_parent(&parent, Some(&allow), &deny, 1)
        .expect("remaining allowed tools should create a registry");

    assert_eq!(child.tool_names(), vec!["read", "run_agent", "search"]);
    assert_eq!(
        warnings,
        vec!["tool 'missing' is not available to the parent agent"]
    );

    let (deep_child, deep_warnings) = create_agent_registry_from_parent(&parent, None, &[], 2)
        .expect("default policy should retain ordinary tools");
    assert!(!deep_child.contains("run_agent"));
    assert!(deep_warnings.is_empty());
}

#[test]
fn tool_result_preserves_error_metadata_and_extra_blocks() {
    let result = ToolResult::error("failed")
        .with_metadata(omini_core::tools::tool_metadata([("kind", json!("test"))]))
        .with_extra_blocks(vec![omini_domain::message::ContentBlock::from_text(
            "extra".into(),
        )]);

    let (block, extra_blocks) = result.into_parts("call-1");
    assert_eq!(block.tool_use_id, "call-1");
    assert!(block.is_error);
    assert_eq!(block.content, "failed");
    assert_eq!(
        block.metadata,
        Some(omini_core::tools::tool_metadata([("kind", json!("test"))]))
    );
    assert_eq!(
        extra_blocks,
        Some(vec![omini_domain::message::ContentBlock::from_text(
            "extra".into()
        )])
    );

    assert_eq!(ToolResult::ok("done").extra_blocks, None);
}

#[tokio::test]
async fn todo_tool_rejects_empty_input_and_serializes_full_list() {
    use omini_core::tools::todo_tool::{TodoItemInput, TodoStatus, TodoWriteInput, TodoWriteTool};

    let empty = TodoWriteTool
        .prepare(TodoWriteInput { todos: Vec::new() })
        .await
        .expect_err("empty todo list should reject");
    assert!(empty.is_error);
    assert_eq!(empty.output, "todos must contain at least one item");
    assert_eq!(empty.metadata, None);
    assert_eq!(empty.extra_blocks, None);

    let input = TodoWriteInput {
        todos: vec![TodoItemInput {
            content: "Implement focused tests".into(),
            status: TodoStatus::InProgress,
        }],
    };
    let prepared = TodoWriteTool
        .prepare(input)
        .await
        .expect("valid todo should prepare");
    assert_eq!(prepared.todos.len(), 1);
    assert_eq!(prepared.todos[0].content, "Implement focused tests");
}
