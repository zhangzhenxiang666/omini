use crate::api::{
    ApiCompletion, ApiEvent, ApiRequest, ApiStream, FinishReason, RequestError, Usage, api_channel,
    send_with_retry, sse::IntoSseStream,
};
use crate::types::message::{ContentBlock, Message, Role, ToolUseBlock};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_stream::StreamExt;

static X_API_KEY: http::HeaderName = http::header::HeaderName::from_static("x-api-key");
static ANTHROPIC_VERSION: (http::HeaderName, http::header::HeaderValue) = (
    http::header::HeaderName::from_static("anthropic-version"),
    http::header::HeaderValue::from_static("2023-06-01"),
);

pub(super) async fn invoke_anthropic(
    http_client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
    request: ApiRequest<'_>,
) -> Result<ApiStream, RequestError> {
    let mut map = Map::new();
    map.insert(
        "model".to_string(),
        Value::String(request.model.to_string()),
    );
    map.insert(
        "messages".to_string(),
        serde_json::to_value(request.messages)?,
    );
    map.insert(
        "max_tokens".to_string(),
        Value::Number(request.max_tokens.unwrap_or(4096).into()),
    );
    map.insert("stream".to_string(), Value::Bool(true));

    if let Some(system_prompt) = request.system_prompt {
        map.insert(
            "system".to_string(),
            Value::String(system_prompt.to_string()),
        );
    }

    if let Some(temperature) = request.temperature {
        map.insert(
            "temperature".to_string(),
            serde_json::to_value(temperature)?,
        );
    }

    // TODO: 还未实现工具定义系统

    let body = Value::Object(map);
    let url = format!("{}/v1/messages", base_url);

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(X_API_KEY.clone(), api_key.parse().unwrap());
    headers.insert(ANTHROPIC_VERSION.0.clone(), ANTHROPIC_VERSION.1.clone());

    let response =
        send_with_retry(|| http_client.post(&url).headers(headers.clone()).json(&body)).await?;

    let (tx, result_stream) = api_channel(256);

    tokio::spawn(async move {
        // ── 当前块状态 ──
        enum BlockState {
            Thinking {
                text: String,
            },
            Text {
                text: String,
            },
            ToolUse {
                id: String,
                name: String,
                partial_input: String,
            },
        }

        let mut current_block: Option<BlockState> = None;
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut prompt_tokens: u32 = 0;
        let mut completion_tokens: u32 = 0;
        let mut finish_reason: FinishReason = FinishReason::Stop;

        let mut stream = Box::pin(
            response
                .bytes_stream()
                .into_sse_stream()
                .timeout(Duration::from_secs(90)),
        );

        // 连续 SSE 解析错误上限
        const MAX_CONSECUTIVE_ERRORS: u32 = 10;
        let mut consecutive_errors: u32 = 0;

        'stream: while let Some(result) = stream.next().await {
            match result {
                Ok(Ok(sse_event)) => {
                    consecutive_errors = 0;

                    // 跳过心跳
                    if sse_event.event == "ping" || sse_event.data.is_empty() {
                        continue;
                    }

                    let data: Value = match serde_json::from_str(&sse_event.data) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(msg = "Failed to parse SSE data", error = %e);
                            continue;
                        }
                    };

                    match sse_event.event.as_str() {
                        "message_start" => {
                            if let Some(msg) = data.get("message")
                                && let Some(usage) = msg.get("usage")
                            {
                                prompt_tokens = usage
                                    .get("input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0)
                                    as u32;
                            }
                        }

                        "content_block_start" => {
                            if let Some(block) = data.get("content_block") {
                                match block.get("type").and_then(|v| v.as_str()) {
                                    Some("text") => {
                                        current_block = Some(BlockState::Text {
                                            text: String::new(),
                                        });
                                    }
                                    Some("thinking") => {
                                        current_block = Some(BlockState::Thinking {
                                            text: String::new(),
                                        });
                                    }
                                    Some("tool_use") => {
                                        let id = block
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let name = block
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        current_block = Some(BlockState::ToolUse {
                                            id,
                                            name,
                                            partial_input: String::new(),
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        }

                        "content_block_delta" => {
                            if let Some(delta) = data.get("delta") {
                                match delta.get("type").and_then(|v| v.as_str()) {
                                    Some("text_delta") => {
                                        if let Some(text) =
                                            delta.get("text").and_then(|v| v.as_str())
                                        {
                                            // 累积到当前 text 块
                                            if let Some(BlockState::Text { text: ref mut acc }) =
                                                current_block
                                            {
                                                acc.push_str(text);
                                            }
                                            // 实时推送给 UI
                                            if tx
                                                .send(Ok(ApiEvent::Text(text.to_string())))
                                                .await
                                                .is_err()
                                            {
                                                break 'stream;
                                            }
                                        }
                                    }
                                    Some("thinking_delta") => {
                                        if let Some(thinking) =
                                            delta.get("thinking").and_then(|v| v.as_str())
                                        {
                                            // 累积到当前 thinking 块
                                            if let Some(BlockState::Thinking {
                                                text: ref mut acc,
                                            }) = current_block
                                            {
                                                acc.push_str(thinking);
                                            }
                                            // 实时推送给 UI
                                            if tx
                                                .send(Ok(ApiEvent::Thinking(thinking.to_string())))
                                                .await
                                                .is_err()
                                            {
                                                break 'stream;
                                            }
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(partial) =
                                            delta.get("partial_json").and_then(|v| v.as_str())
                                            && let Some(BlockState::ToolUse {
                                                ref mut partial_input,
                                                ..
                                            }) = current_block
                                        {
                                            partial_input.push_str(partial);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }

                        "content_block_stop" => {
                            if let Some(block) = current_block.take() {
                                match block {
                                    BlockState::Text { text } => {
                                        content_blocks.push(ContentBlock::from_text(text));
                                    }
                                    BlockState::Thinking { text } => {
                                        content_blocks.push(ContentBlock::from_thinking(text));
                                    }
                                    BlockState::ToolUse {
                                        id,
                                        name,
                                        partial_input,
                                    } => {
                                        let input: HashMap<String, Value> =
                                            serde_json::from_str(&partial_input)
                                                .unwrap_or_default();
                                        let tool_use = ToolUseBlock { id, name, input };
                                        content_blocks
                                            .push(ContentBlock::ToolUse(tool_use.clone()));

                                        if tx.send(Ok(ApiEvent::ToolUse(tool_use))).await.is_err() {
                                            break 'stream;
                                        }
                                    }
                                }
                            }
                        }

                        "message_delta" => {
                            if let Some(usage) = data.get("usage") {
                                completion_tokens = usage
                                    .get("output_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0)
                                    as u32;
                            }
                            if let Some(delta) = data.get("delta") {
                                finish_reason = delta
                                    .get("stop_reason")
                                    .and_then(|v| v.as_str())
                                    .map(|r| match r {
                                        "end_turn" | "stop_sequence" => FinishReason::Stop,
                                        "max_tokens" => FinishReason::Length,
                                        "tool_use" => FinishReason::ToolUse,
                                        other => FinishReason::Error(other.to_string()),
                                    })
                                    .unwrap_or(FinishReason::Stop);
                            }
                        }

                        "message_stop" => {
                            // 将所有已累积的内容块封装进 Done
                            let completion = ApiCompletion {
                                message: Message::new(
                                    Role::Assistant,
                                    std::mem::take(&mut content_blocks),
                                ),
                                finish_reason: finish_reason.clone(),
                                usage: Usage {
                                    prompt_tokens: prompt_tokens as usize,
                                    completion_tokens: completion_tokens as usize,
                                },
                            };
                            if tx.send(Ok(ApiEvent::Done(completion))).await.is_err() {
                                break 'stream;
                            }
                        }

                        _ => {
                            tracing::debug!(
                                msg = "Unknown SSE event",
                                event = %sse_event.event,
                            );
                        }
                    }
                }

                // -- SSE 解析层错误 --
                Ok(Err(err)) => {
                    consecutive_errors += 1;
                    tracing::warn!(
                        msg = "SSE parse error",
                        error = %err,
                        consecutive_errors,
                        max = MAX_CONSECUTIVE_ERRORS,
                    );
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        tracing::error!("Too many consecutive SSE errors, stopping stream");
                        break 'stream;
                    }
                    continue;
                }

                // -- 超时：90s 内没有收到任何数据 --
                Err(_elapsed) => {
                    tracing::warn!("SSE stream timed out after 90s");
                    break 'stream;
                }
            }
        }

        // tx 在此 drop，通知接收端流已结束
        tracing::debug!("SSE stream task finished, channel closed");
    });
    Ok(result_stream)
}
