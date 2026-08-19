mod support;

use omini_domain::config::{ProviderEndpointKind, ThinkingEffort};
use omini_domain::message::{ContentBlock, Message, Role};
use omini_domain::tool::ToolDefinition;
use omini_provider_api::{ApiEvent, ApiRequest, FinishReason, RequestError, StreamError};
use serde_json::{Map, json};
use std::collections::HashMap;
use tokio_stream::StreamExt;

use crate::support::{TestResponse, TestServer, client};

fn request<'a>(messages: &'a [Message]) -> ApiRequest<'a> {
    ApiRequest {
        messages,
        model: "test-model",
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
async fn openai_request_complete_context_projects_provider_shape_and_overrides() {
    let server = TestServer::spawn(vec![TestResponse::sse(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )]);
    let mut tool_input = HashMap::new();
    tool_input.insert("path".to_string(), json!("src/lib.rs"));
    let messages = vec![
        Message::new(
            Role::User,
            vec![
                ContentBlock::from_tool_result("call_1".into(), false, "loaded".into()),
                ContentBlock::from_text("look".into()),
                ContentBlock::from_base64_image("image/png".into(), "aGVsbG8=".into()),
            ],
        ),
        Message::new(
            Role::Assistant,
            vec![
                ContentBlock::from_thinking("reasoning".into()),
                ContentBlock::from_text("answer".into()),
                ContentBlock::from_tool_use("call_2".into(), "read_file".into(), tool_input),
            ],
        ),
    ];
    let tools = vec![ToolDefinition {
        name: "read_file".into(),
        description: "Read one file".into(),
        input_schema: json!({"type": "object", "required": ["path"]}),
    }];
    let extra_headers = HashMap::from([
        (
            "authorization".to_string(),
            "Bearer override-key".to_string(),
        ),
        ("x-provider-feature".to_string(), "enabled".to_string()),
        ("bad\nheader".to_string(), "ignored".to_string()),
    ]);
    let extra_body = Map::from_iter([
        ("model".to_string(), json!("compat-model")),
        ("routing_mode".to_string(), json!("fast")),
    ]);
    let request = ApiRequest {
        messages: &messages,
        model: "test-model",
        system_prompt: Some("be concise"),
        tools: Some(&tools),
        max_tokens: Some(123),
        temperature: Some(0.25),
        thinking_effort: Some(ThinkingEffort::Max),
        extra_headers: Some(&extra_headers),
        extra_body: Some(&extra_body),
    };

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request)
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    let recorded = server.next_request();
    server.finish();

    assert!(matches!(events.as_slice(), [Ok(ApiEvent::Done(_))]));
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.path, "/chat/completions");
    assert_eq!(recorded.headers["authorization"], "Bearer override-key");
    assert_eq!(recorded.headers["x-provider-feature"], "enabled");
    assert!(!recorded.headers.contains_key("bad\nheader"));
    assert_eq!(
        recorded.body,
        json!({
            "model": "compat-model",
            "messages": [
                {"role": "system", "content": "be concise"},
                {"role": "tool", "tool_call_id": "call_1", "content": "loaded"},
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "look"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}}
                    ]
                },
                {
                    "role": "assistant",
                    "content": "answer",
                    "reasoning_content": "reasoning",
                    "tool_calls": [{
                        "id": "call_2",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"src/lib.rs\"}"}
                    }]
                }
            ],
            "stream": true,
            "stream_options": {"include_usage": true},
            "max_tokens": 123,
            "reasoning_effort": "high",
            "temperature": 0.25,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read one file",
                    "parameters": {"type": "object", "required": ["path"]}
                }
            }],
            "routing_mode": "fast"
        })
    );
}

#[tokio::test]
async fn openai_stream_complete_response_preserves_event_and_tool_order() {
    let server = TestServer::spawn(vec![TestResponse::sse(concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"answer \",\"reasoning_content\":\"think\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_0\",\"function\":{\"name\":\"first\",\"arguments\":\"{\\\"a\\\":1}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_1\",\"function\":{\"name\":\"second\",\"arguments\":\"{\\\"b\\\":2}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":16}}}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    ))]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 5);
    assert!(matches!(&events[0], Ok(ApiEvent::Text(text)) if text == "answer "));
    assert!(matches!(&events[1], Ok(ApiEvent::Thinking(text)) if text == "think"));
    // OpenAI 没有单独的 tool stop：索引 1 首次出现时，索引 0 才能安全地按顺序派发。
    assert!(
        matches!(&events[2], Ok(ApiEvent::ToolUse(tool)) if tool.id == "call_0" && tool.name == "first" && tool.input == HashMap::from([(String::from("a"), json!(1))]))
    );
    assert!(
        matches!(&events[3], Ok(ApiEvent::ToolUse(tool)) if tool.id == "call_1" && tool.name == "second" && tool.input == HashMap::from([(String::from("b"), json!(2))]))
    );
    let Ok(ApiEvent::Done(done)) = &events[4] else {
        panic!("stream should finish with a completion: {events:?}");
    };
    assert!(matches!(done.finish_reason, FinishReason::ToolUse));
    assert_eq!(done.usage.prompt_tokens, 42);
    assert_eq!(done.usage.completion_tokens, 7);
    assert_eq!(done.usage.cached_tokens, 16);
    assert_eq!(
        done.message.content,
        vec![
            ContentBlock::from_text("answer ".into()),
            ContentBlock::from_thinking("think".into()),
            ContentBlock::from_tool_use(
                "call_0".into(),
                "first".into(),
                HashMap::from([(String::from("a"), json!(1))])
            ),
            ContentBlock::from_tool_use(
                "call_1".into(),
                "second".into(),
                HashMap::from([(String::from("b"), json!(2))])
            ),
        ]
    );
}

#[tokio::test]
async fn openai_stream_invalid_json_returns_json_error_without_completion() {
    let server = TestServer::spawn(vec![TestResponse::sse("data: {invalid}\n\n")]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(StreamError::Json(_))));
}

#[tokio::test]
async fn openai_stream_missing_tool_index_returns_unexpected_end() {
    let server = TestServer::spawn(vec![TestResponse::sse(concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_1\",\"function\":{\"name\":\"second\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    ))]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(StreamError::UnexpectedEnd)));
}

#[tokio::test]
async fn openai_stream_invalid_tool_arguments_returns_json_error_without_completion() {
    let server = TestServer::spawn(vec![TestResponse::sse(concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_0\",\"function\":{\"name\":\"broken\",\"arguments\":\"{invalid\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    ))]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(StreamError::Json(_))));
}

#[tokio::test]
async fn openai_stream_without_done_sentinel_returns_unexpected_end() {
    let server = TestServer::spawn(vec![TestResponse::sse(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    )]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    server.next_request();
    server.finish();

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], Err(StreamError::UnexpectedEnd)));
}

#[tokio::test]
async fn openai_request_missing_optional_fields_uses_default_token_limit() {
    let server = TestServer::spawn(vec![TestResponse::sse(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    let recorded = server.next_request();
    server.finish();

    assert!(matches!(events.as_slice(), [Ok(ApiEvent::Done(_))]));
    assert_eq!(recorded.body["max_tokens"], json!(32768));
    assert!(recorded.body.get("reasoning_effort").is_none());
    assert!(recorded.body.get("temperature").is_none());
    assert!(recorded.body.get("tools").is_none());
}

#[tokio::test]
async fn openai_request_non_success_status_preserves_status_and_body() {
    let server = TestServer::spawn(vec![TestResponse::status(
        400,
        "Bad Request",
        "invalid model",
    )]);
    let messages = vec![Message::from_user_text("hello".into())];

    let error = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect_err("400 response should reject request");
    let recorded = server.next_request();
    server.finish();

    assert_eq!(recorded.path, "/chat/completions");
    let RequestError::Api { status, body } = error else {
        panic!("expected API status error: {error:?}");
    };
    assert_eq!(status, http::StatusCode::BAD_REQUEST);
    assert_eq!(body, "invalid model");
}
