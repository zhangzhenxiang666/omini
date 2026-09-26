use omini_model::message::{ContentBlock, Message, Role, ThinkingBlock, ToolResultBlock};
use serde_json::json;
use std::collections::HashMap;

#[test]
fn content_block_constructors_create_the_matching_variant() {
    let thinking = ContentBlock::from_thinking("reason".into());
    let text = ContentBlock::from_text("answer".into());
    let image = ContentBlock::from_base64_image("image/png".into(), "AAEC".into());
    let tool_use = ContentBlock::from_tool_use(
        "tool-1".into(),
        "read".into(),
        HashMap::from([("path".into(), json!("src/lib.rs"))]),
    );
    let tool_result = ContentBlock::from_tool_result("tool-1".into(), false, "contents".into());

    assert!(thinking.is_thinking());
    assert!(text.is_text());
    assert!(image.is_image());
    assert!(tool_use.is_tool_use());
    assert!(tool_result.is_tool_result());

    for (block, predicates) in [
        (thinking, [false, false, false, false, true]),
        (text, [true, false, false, false, false]),
        (image, [false, true, false, false, false]),
        (tool_use, [false, false, true, false, false]),
        (tool_result, [false, false, false, true, false]),
    ] {
        assert_eq!(
            [
                block.is_text(),
                block.is_image(),
                block.is_tool_use(),
                block.is_tool_result(),
                block.is_thinking(),
            ],
            predicates
        );
    }
}

#[test]
fn all_content_block_variants_round_trip_with_tagged_json() {
    let cases = [
        json!({"type": "thinking", "thinking": "reason"}),
        json!({"type": "text", "text": "answer"}),
        json!({
            "type": "image",
            "source": {"type": "base64", "media_type": "image/jpeg", "data": "abc"}
        }),
        json!({"type": "tool_use", "id": "t1", "name": "read", "input": {"path": "a"}}),
        json!({"type": "tool_result", "tool_use_id": "t1", "is_error": false, "content": "ok"}),
    ];

    for value in cases {
        let block: ContentBlock =
            serde_json::from_value(value.clone()).expect("valid block should deserialize");
        assert_eq!(
            serde_json::to_value(block).expect("block should serialize"),
            value
        );
    }
}

#[test]
fn tool_result_metadata_defaults_to_none_and_is_omitted() {
    let value = json!({
        "type": "tool_result",
        "tool_use_id": "t1",
        "is_error": true,
        "content": "failed"
    });

    let block: ContentBlock =
        serde_json::from_value(value.clone()).expect("tool result should deserialize");
    assert!(matches!(
        &block,
        ContentBlock::ToolResult(ToolResultBlock { metadata: None, .. })
    ));
    assert_eq!(
        serde_json::to_value(block).expect("tool result should serialize"),
        value
    );
}

#[test]
fn tool_result_nonempty_metadata_is_preserved() {
    let value = json!({
        "type": "tool_result",
        "tool_use_id": "t1",
        "is_error": false,
        "content": "ok",
        "metadata": {"exit_code": 0, "nested": {"cached": true}}
    });

    let block: ContentBlock =
        serde_json::from_value(value.clone()).expect("metadata should deserialize");
    assert_eq!(
        serde_json::to_value(block).expect("metadata should serialize"),
        value
    );
}

#[test]
fn thinking_duration_defaults_to_none_and_is_omitted() {
    // 旧持久化记录没有 duration_ms 字段，必须能反序列化为 None 且序列化时省略
    let value = json!({"type": "thinking", "thinking": "reason"});

    let block: ContentBlock =
        serde_json::from_value(value.clone()).expect("thinking should deserialize");
    assert!(matches!(
        &block,
        ContentBlock::Thinking(ThinkingBlock {
            duration_ms: None,
            ..
        })
    ));
    assert_eq!(
        serde_json::to_value(block).expect("thinking should serialize"),
        value
    );
}

#[test]
fn thinking_duration_is_preserved_when_present() {
    let value = json!({
        "type": "thinking",
        "thinking": "reason",
        "duration_ms": 5300
    });

    let block: ContentBlock =
        serde_json::from_value(value.clone()).expect("duration should deserialize");
    assert!(matches!(
        &block,
        ContentBlock::Thinking(ThinkingBlock {
            duration_ms: Some(5300),
            ..
        })
    ));
    assert_eq!(
        serde_json::to_value(block).expect("duration should serialize"),
        value
    );
}

#[test]
fn invalid_content_block_tags_or_required_fields_are_rejected() {
    for value in [
        json!({"type": "audio", "data": "abc"}),
        json!({"type": "text"}),
        json!({
            "type": "image",
            "source": {"type": "url", "media_type": "image/png", "data": "abc"}
        }),
        json!({"type": "tool_use", "id": "t1", "name": "read"}),
    ] {
        assert!(
            serde_json::from_value::<ContentBlock>(value).is_err(),
            "invalid content block should be rejected"
        );
    }
}

#[test]
fn message_helpers_preserve_role_content_and_display_names() {
    let user = Message::from_user_text("hello".into());
    assert_eq!(user.role, Role::User);
    assert_eq!(user.content, vec![ContentBlock::from_text("hello".into())]);

    let assistant = Message::new(Role::Assistant, vec![ContentBlock::from_text("hi".into())]);
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(Role::User.to_string(), "user");
    assert_eq!(Role::Assistant.to_string(), "assistant");
    assert_eq!(
        serde_json::to_value(assistant).expect("message should serialize"),
        json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}]
        })
    );
}
