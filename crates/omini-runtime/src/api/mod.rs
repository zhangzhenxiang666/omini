use crate::types::config::ProviderType;
use crate::types::config::ThinkingEffort;
use crate::types::message::{Message, ToolUseBlock};
use crate::types::tool::ToolDefinition;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub mod anthropic;
pub mod openai;
pub mod sse;

#[derive(Debug, Clone)]
pub struct LlmClient {
    pub(crate) http_client: &'static reqwest::Client,
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) protocol: ProviderType,
}

impl LlmClient {
    pub fn new(protocol: ProviderType, api_key: String, base_url: String) -> Self {
        Self {
            http_client: crate::util::http_client(),
            api_key,
            base_url,
            protocol,
        }
    }

    /// 命令系统切换模型时直接替换字段
    pub fn switch(&mut self, protocol: ProviderType, api_key: String, base_url: String) {
        self.protocol = protocol;
        self.api_key = api_key;
        self.base_url = base_url;
    }

    /// 调用 LLM，内部按 protocol 分发
    pub async fn invoke(&self, request: ApiRequest<'_>) -> Result<ApiStream, RequestError> {
        match self.protocol {
            ProviderType::OpenAI => {
                openai::invoke_openai(self.http_client, &self.api_key, &self.base_url, request)
                    .await
            }
            ProviderType::Anthropic => {
                anthropic::invoke_anthropic(
                    self.http_client,
                    &self.api_key,
                    &self.base_url,
                    request,
                )
                .await
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiRequest<'a> {
    pub messages: &'a [Message],
    pub model: &'a str,
    pub system_prompt: Option<&'a str>,
    pub tools: Option<&'a [ToolDefinition]>,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub thinking_effort: Option<ThinkingEffort>,
}

#[derive(Debug, Clone)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
}

#[derive(Debug, Clone)]
pub enum FinishReason {
    Stop,
    Length,
    ToolUse,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ApiCompletion {
    pub message: Message,
    pub usage: Usage,
    pub finish_reason: FinishReason,
}

#[derive(Debug, Clone)]
pub enum ApiEvent {
    Thinking(String),
    Text(String),
    ToolUse(ToolUseBlock),
    Done(ApiCompletion),
}

/// 请求阶段错误 —— Provider 内部据此判断是否重试（429、5xx 等）
#[derive(Debug, Error)]
pub enum RequestError {
    /// HTTP 传输层错误（连接被拒绝、超时、TLS 等）
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// API 返回了非成功状态码
    #[error("API error: {status} - {body}")]
    Api {
        status: http::StatusCode,
        body: String,
    },

    /// JSON 序列化错误（构建请求 body 时）
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 流阶段错误 —— 进入 SSE 流之后发生的终端错误，不再重试
#[derive(Debug, Error)]
pub enum StreamError {
    /// SSE 流解析错误
    #[error("SSE parse error: {0}")]
    Sse(String),

    /// JSON 反序列化错误（解析响应事件时）
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// 内部 mpsc 接收端被丢弃
    #[error("Internal channel closed")]
    ChannelClosed,

    /// SSE 流在收到完整响应前意外结束
    #[error("Stream ended unexpectedly")]
    UnexpectedEnd,
}

pub type ApiStream = ReceiverStream<Result<ApiEvent, StreamError>>;

/// 最大重试次数（含首次请求后的重试）
pub const MAX_RETRIES: usize = 5;

/// 判断 HTTP 状态码是否可重试（429 或 5xx）
pub fn is_retryable(status: http::StatusCode) -> bool {
    status == http::StatusCode::TOO_MANY_REQUESTS || status.as_u16() >= 500
}

/// 指数退避延迟（毫秒）：1s, 2s, 4s, 8s, 16s
fn backoff_ms(attempt: usize) -> u64 {
    1000 * 2u64.pow(attempt as u32)
}

/// 发送请求并自动重试可恢复的错误（429、5xx）。
///
/// `build_request` 在每次重试时都会被调用，因此应返回一个全新的 `RequestBuilder`。
/// 请求阶段的所有错误（网络层、非可重试状态码）直接返回 `RequestError`。
pub async fn send_with_retry(
    build_request: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, RequestError> {
    let mut last_error = None;

    for attempt in 0..=MAX_RETRIES {
        let request = build_request();

        match request.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }

                let body = resp.text().await.unwrap_or_default();
                let err = RequestError::Api { status, body };

                if is_retryable(status) && attempt < MAX_RETRIES {
                    last_error = Some(err);
                    tokio::time::sleep(Duration::from_millis(backoff_ms(attempt))).await;
                    continue;
                }

                return Err(err);
            }
            Err(e) => {
                let err = RequestError::Http(e);
                if attempt < MAX_RETRIES {
                    last_error = Some(err);
                    tokio::time::sleep(Duration::from_millis(backoff_ms(attempt))).await;
                    continue;
                }
                return Err(err);
            }
        }
    }

    Err(last_error.unwrap())
}

pub fn api_channel(buffer: usize) -> (mpsc::Sender<Result<ApiEvent, StreamError>>, ApiStream) {
    let (tx, rx) = mpsc::channel(buffer);
    (tx, ReceiverStream::new(rx))
}
