mod support;

use omini_domain::config::{ProviderEndpointKind, ThinkingEffort};
use omini_domain::message::{ContentBlock, Message, Role, ThinkingBlock, ToolResultBlock};
use omini_domain::tool::ToolDefinition;
use omini_provider_api::{ApiEvent, ApiRequest, FinishReason, StreamError};
use serde_json::{Map, json};
use std::collections::HashMap;
use tokio_stream::StreamExt;

use crate::support::{TestResponse, TestServer, client};

fn request<'a>(messages: &'a [Message]) -> ApiRequest<'a> {
    ApiRequest {
        messages,
        model: "claude-test",
        system_prompt: None,
        tools: None,
        max_tokens: None,
        temperature: None,
        thinking_effort: None,
        extra_headers: None,
        extra_body: None,
    }
}

#[tokio::test]
async fn anthropic_request_default_fields_cache_last_block_and_strip_tool_metadata() {    let server = TestServer::spawn(vec![TestResponse::sse("event: message_stop\ndata: {}\n\n")]);
    let metadata = Map::from_iter([(String::from("permission_denied"), json!(true))]);
    let messages = vec![Message::new(
        Role::User,
        vec![
            ContentBlock::from_tool_use("toolu_1".into(), "read_file".into(), HashMap::new()),
            ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: "toolu_1".into(),
                is_error: true,
                content: "denied".into(),
                metadata: Some(metadata),
            }),
        ],
    )];

    let events = client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    let recorded = server.next_request();
    server.finish();

    assert!(matches!(events.as_slice(), [Ok(ApiEvent::Done(_))]));
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/v1/messages");
    assert_eq!(recorded.headers["x-api-key"], "test-api-key");
    assert_eq!(recorded.headers["anthropic-version"], "2023-06-01");
    assert_eq!(recorded.body["model"], json!("claude-test"));
    assert_eq!(recorded.body["stream"], json!(true));
    assert_eq!(recorded.body["max_tokens"], json!(32768));
    assert!(recorded.body.get("system").is_none());
    assert!(recorded.body.get("thinking").is_none());
    assert!(recorded.body.get("temperature").is_none());
    assert!(recorded.body.get("tools").is_none());
    let content = recorded.body["messages"][0]["content"]
        .as_array()
        .expect("message content should be an array");
    assert_eq!(content.len(), 2);
    assert!(content[0].get("cache_control").is_none());
    assert!(content[1].get("metadata").is_none());
    assert_eq!(content[1]["cache_control"], json!({"type": "ephemeral"}));
}

#[tokio::test]
async fn anthropic_request_strips_thinking_duration_ms() {
    // duration_ms 是本地 UI 显示元数据，不允许泄漏进 Anthropic API 请求体
    let server = TestServer::spawn(vec![TestResponse::sse("event: message_stop\ndata: {}\n\n")]);
    let messages = vec![Message::new(
        Role::Assistant,
        vec![ContentBlock::Thinking(ThinkingBlock {
            thinking: "reasoning".into(),
            duration_ms: Some(5300),
        })],
    )];

    client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    let recorded = server.next_request();
    server.finish();

    let content = recorded.body["messages"][0]["content"]
        .as_array()
        .expect("message content should be an array");
    assert_eq!(content[0]["type"], json!("thinking"));
    assert_eq!(content[0]["thinking"], json!("reasoning"));
    assert!(content[0].get("duration_ms").is_none());
}

#[tokio::test]
async fn anthropic_request_optional_fields_and_extra_values_override_provider_defaults() {
    let server = TestServer::spawn(vec![TestResponse::sse("event: message_stop\ndata: {}\n\n")]);
    let messages = vec![Message::from_user_text("hello".into())];
    let tools = vec![ToolDefinition {
        name: "search".into(),
        description: "Search docs".into(),
        input_schema: json!({"type": "object"}),
    }];
    let headers = HashMap::from([("x-api-key".to_string(), "override-key".to_string())]);
    let body = Map::from_iter([
        ("max_tokens".to_string(), json!(7)),
        ("custom_mode".to_string(), json!("fast")),
    ]);
    let request = ApiRequest {
        messages: &messages,
        model: "claude-test",
        system_prompt: Some("system prompt"),
        tools: Some(&tools),
        max_tokens: Some(99),
        temperature: Some(0.5),
        thinking_effort: Some(ThinkingEffort::High),
        extra_headers: Some(&headers),
        extra_body: Some(&body),
    };

    let stream = client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request)
        .await
        .expect("Anthropic request should start");
    drop(stream);
    let recorded = server.next_request();
    server.finish();

    assert_eq!(recorded.headers["x-api-key"], "override-key");
    assert_eq!(
        recorded.body["system"],
        json!([{
            "type": "text",
            "text": "system prompt",
            "cache_control": {"type": "ephemeral"}
        }])
    );
    assert_eq!(recorded.body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(recorded.body["output_config"], json!({"effort": "high"}));
    assert_eq!(recorded.body["temperature"], json!(0.5));
    assert_eq!(recorded.body["tools"], json!(tools));
    assert_eq!(recorded.body["max_tokens"], json!(7));
    assert_eq!(recorded.body["custom_mode"], json!("fast"));
}

#[tokio::test]
async fn anthropic_stream_complete_response_preserves_blocks_usage_and_finish_reason() {
    let server = TestServer::spawn(vec![TestResponse::sse(concat!(
        "event: ping\ndata: {}\n\n",
        "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":100,\"cache_creation_input_tokens\":25,\"cache_read_input_tokens\":75}}}\n\n",
        "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\"}}\n\n",
        "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
        "event: content_block_stop\ndata: {}\n\n",
        "event: content_block_start\ndata: {\"content_block\":{\"type\":\"thinking\"}}\n\n",
        "event: content_block_delta\ndata: {\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"reason\"}}\n\n",
        "event: content_block_stop\ndata: {}\n\n",
        "event: content_block_start\ndata: {\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"search\"}}\n\n",
        "event: content_block_delta\ndata: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"omini\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {}\n\n",
        "event: message_delta\ndata: {\"usage\":{\"input_tokens\":0,\"output_tokens\":30},\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
        "event: message_stop\ndata: {}\n\n"
    ))]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 4);
    assert!(matches!(&events[0], Ok(ApiEvent::Text(text)) if text == "answer"));
    assert!(matches!(&events[1], Ok(ApiEvent::Thinking(text)) if text == "reason"));
    assert!(
        matches!(&events[2], Ok(ApiEvent::ToolUse(tool)) if tool.id == "toolu_1" && tool.name == "search" && tool.input == HashMap::from([(String::from("query"), json!("omini"))]))
    );
    let Ok(ApiEvent::Done(done)) = &events[3] else {
        panic!("stream should finish with a completion: {events:?}");
    };
    assert!(matches!(done.finish_reason, FinishReason::ToolUse));
    assert_eq!(done.usage.prompt_tokens, 200);
    assert_eq!(done.usage.completion_tokens, 30);
    assert_eq!(done.usage.cached_tokens, 75);
    assert_eq!(
        done.message.content,
        vec![
            ContentBlock::from_text("answer".into()),
            ContentBlock::from_thinking("reason".into()),
            ContentBlock::from_tool_use(
                "toolu_1".into(),
                "search".into(),
                HashMap::from([(String::from("query"), json!("omini"))]),
            ),
        ]
    );
}

#[tokio::test]
async fn anthropic_stream_invalid_tool_json_returns_json_error_without_tool_event() {
    let server = TestServer::spawn(vec![TestResponse::sse(concat!(
        "event: content_block_start\ndata: {\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"search\"}}\n\n",
        "event: content_block_delta\ndata: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{invalid\"}}\n\n",
        "event: content_block_stop\ndata: {}\n\n"
    ))]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(StreamError::Json(_))));
}

#[tokio::test]
async fn anthropic_stream_without_message_stop_returns_unexpected_end() {
    let server = TestServer::spawn(vec![TestResponse::sse(
        "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
    )]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(StreamError::UnexpectedEnd)));
}

#[tokio::test]
// 回归：多字节字符被 TCP chunk 边界拆开时，文本必须无损送达（不能出现 U+FFFD）。
async fn anthropic_stream_multibyte_text_survives_split_across_tcp_chunks() {
    let body = concat!(
        "event: message_start\ndata: {}\n\n",
        "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\"}}\n\n",
        "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"账号规模化\"}}\n\n",
        "event: content_block_stop\ndata: {}\n\n",
        "event: message_stop\ndata: {}\n\n"
    );
    // 在 “规” 的首字节之后切分，模拟网络分块落在多字节字符中间
    let split = body
        .as_bytes()
        .windows("规".len())
        .position(|window| window == "规".as_bytes())
        .expect("body should contain the multibyte character")
        + 1;
    let server = TestServer::spawn(vec![TestResponse::sse_chunked(body.as_bytes(), &[split])]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], Ok(ApiEvent::Text(text)) if text == "账号规模化"));
    let Ok(ApiEvent::Done(done)) = &events[1] else {
        panic!("stream should finish with a completion: {events:?}");
    };
    assert_eq!(
        done.message.content,
        vec![ContentBlock::from_text("账号规模化".into())]
    );
}

#[tokio::test]
// 反向：响应含非法 UTF-8 时必须以 SSE 错误终止，不能 lossy 解码后继续产出乱码。
async fn anthropic_stream_invalid_utf8_returns_sse_error_without_events() {
    let mut body = concat!(
        "event: message_start\ndata: {}\n\n",
        "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\"}}\n\n"
    )
    .as_bytes()
    .to_vec();
    body.extend_from_slice(
        b"event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"bad \xff byte\"}}\n\n",
    );
    let server = TestServer::spawn(vec![TestResponse::sse_chunked(&body, &[])]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::Anthropic, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert!(matches!(
        events.as_slice(),
        [Err(StreamError::Sse(message))] if message.contains("not valid UTF-8")
    ));
}
