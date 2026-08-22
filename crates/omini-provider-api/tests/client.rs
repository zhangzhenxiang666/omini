mod support;

use omini_domain::config::ProviderEndpointKind;
use omini_domain::message::Message;
use omini_provider_api::{ApiEvent, ApiRequest, is_retryable};
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

#[test]
fn retryable_status_429_and_5xx_are_retryable_while_other_client_errors_are_not() {
    for (status, expected) in [
        (http::StatusCode::BAD_REQUEST, false),
        (http::StatusCode::NOT_FOUND, false),
        (http::StatusCode::TOO_MANY_REQUESTS, true),
        (http::StatusCode::INTERNAL_SERVER_ERROR, true),
        (http::StatusCode::from_u16(599).expect("599 is valid"), true),
    ] {
        assert_eq!(is_retryable(status), expected, "{status} retryability");
    }
}

#[tokio::test]
async fn openai_request_retryable_failure_retries_once_and_returns_later_completion() {
    let server = TestServer::spawn(vec![
        TestResponse::status(500, "Internal Server Error", "retry"),
        TestResponse::sse(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ),
    ]);
    let messages = vec![Message::from_user_text("hello".into())];

    let events = client(ProviderEndpointKind::OpenAI, server.base_url())
        .invoke(request(&messages))
        .await
        .expect("retryable failure should be retried")
        .collect::<Vec<_>>()
        .await;
    let first = server.next_request();
    let second = server.next_request();
    server.finish();

    assert_eq!(first.path, "/chat/completions");
    assert_eq!(second.path, "/chat/completions");
    assert!(matches!(events.as_slice(), [Ok(ApiEvent::Done(_))]));
}

#[tokio::test]
async fn llm_client_switch_changes_protocol_for_subsequent_requests() {
    let openai = TestServer::spawn(vec![TestResponse::sse(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )]);
    let anthropic = TestServer::spawn(vec![TestResponse::sse("event: message_stop\ndata: {}\n\n")]);
    let messages = vec![Message::from_user_text("hello".into())];
    let mut llm_client = client(ProviderEndpointKind::OpenAI, openai.base_url());

    let openai_events = llm_client
        .invoke(request(&messages))
        .await
        .expect("OpenAI request should start")
        .collect::<Vec<_>>()
        .await;
    llm_client.switch(
        ProviderEndpointKind::Anthropic,
        "second-key".into(),
        url::Url::parse(anthropic.base_url()).unwrap(),
    );
    let anthropic_events = llm_client
        .invoke(request(&messages))
        .await
        .expect("Anthropic request should start")
        .collect::<Vec<_>>()
        .await;
    let openai_request = openai.next_request();
    openai.finish();
    let anthropic_request = anthropic.next_request();
    anthropic.finish();

    assert_eq!(openai_request.path, "/chat/completions");
    assert_eq!(anthropic_request.path, "/v1/messages");
    assert_eq!(anthropic_request.headers["x-api-key"], "second-key");
    assert!(matches!(openai_events.as_slice(), [Ok(ApiEvent::Done(_))]));
    assert!(matches!(
        anthropic_events.as_slice(),
        [Ok(ApiEvent::Done(_))]
    ));
}
