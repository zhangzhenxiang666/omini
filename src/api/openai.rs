use crate::api::{
    ApiCompletion, ApiEvent, ApiRequest, ApiStream, FinishReason, RequestError, StreamError, Usage,
    api_channel, send_with_retry, sse::IntoSseStream,
};
use crate::types::config::ThinkingEffort;
use crate::types::message::{ContentBlock, Message, Role, ToolResultBlock, ToolUseBlock};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_stream::StreamExt;

pub(super) async fn invoke_openai(
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
    let openai_messages = convert_messages_to_openai(request.messages, request.system_prompt);
    map.insert("messages".to_string(), Value::Array(openai_messages));
    map.insert("stream".to_string(), Value::Bool(true));
    map.insert(
        "stream_options".to_string(),
        serde_json::json!({ "include_usage": true }),
    );

    let max_tokens = request.max_tokens.unwrap_or(32768);
    map.insert("max_tokens".to_string(), Value::Number(max_tokens.into()));

    if let Some(effort) = request.thinking_effort
        && effort != ThinkingEffort::None
    {
        map.insert(
            "reasoning_effort".to_string(),
            serde_json::to_value(effort)?,
        );
    }

    if let Some(temperature) = request.temperature {
        map.insert(
            "temperature".to_string(),
            serde_json::to_value(temperature)?,
        );
    }

    if let Some(tools) = request.tools {
        let tools_value: Vec<Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        map.insert("tools".to_string(), Value::Array(tools_value));
    }

    let body = Value::Object(map);
    let url = format!("{}/chat/completions", base_url);

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        format!("Bearer {}", api_key).parse().unwrap(),
    );

    let response =
        send_with_retry(|| http_client.post(&url).headers(headers.clone()).json(&body)).await?;

    let (tx, result_stream) = api_channel(256);

    tokio::spawn(async move {
        // 文本累积（OpenAI 的 content 不分块类型，统一累积）
        let mut accumulated_text: Option<String> = None;
        // 思考内容累积（reasoning_content）
        let mut accumulated_reasoning: Option<String> = None;
        // 工具调用累积（按 tool_call.index 索引）
        let mut tool_calls: HashMap<usize, ToolCallAcc> = HashMap::new();
        let mut next_expected_tool_index: usize = 0;
        // 最终组装
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut prompt_tokens: u32 = 0;
        let mut completion_tokens: u32 = 0;
        let mut finish_reason: FinishReason = FinishReason::Stop;
        let mut saw_finish_reason = false;
        let mut saw_done = false;

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

                    // OpenAI 用 [DONE] 结尾
                    if sse_event.data == "[DONE]" {
                        saw_done = true;
                        break 'stream;
                    }

                    if sse_event.data.is_empty() {
                        continue;
                    }

                    let data: Value = match serde_json::from_str(&sse_event.data) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(msg = "Failed to parse SSE data", error = %e);
                            let _ = tx.send(Err(StreamError::Json(e))).await;
                            return;
                        }
                    };

                    let choice = match data
                        .get("choices")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.first())
                    {
                        Some(c) => c,
                        None => continue,
                    };

                    // finish_reason
                    let current_finish_reason = choice
                        .get("finish_reason")
                        .and_then(|v| v.as_str())
                        .filter(|r| !r.is_empty());
                    if let Some(reason) = current_finish_reason {
                        saw_finish_reason = true;
                        finish_reason = match reason {
                            "stop" => FinishReason::Stop,
                            "length" => FinishReason::Length,
                            "tool_calls" => FinishReason::ToolUse,
                            other => FinishReason::Error(other.to_string()),
                        };
                    }

                    // usage
                    if let Some(usage) = data.get("usage") {
                        prompt_tokens = usage
                            .get("prompt_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                        completion_tokens = usage
                            .get("completion_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32;
                    }

                    if let Some(delta) = choice.get("delta") {
                        // content delta
                        if let Some(text) = delta
                            .get("content")
                            .and_then(|v| v.as_str())
                            .filter(|t| !t.is_empty())
                        {
                            accumulated_text
                                .get_or_insert_with(String::new)
                                .push_str(text);

                            if tx.send(Ok(ApiEvent::Text(text.to_string()))).await.is_err() {
                                return;
                            }
                        }

                        // reasoning_content delta
                        if let Some(thinking) = delta
                            .get("reasoning_content")
                            .or_else(|| delta.get("reasoning"))
                            .and_then(|v| v.as_str())
                            .filter(|t| !t.is_empty())
                        {
                            accumulated_reasoning
                                .get_or_insert_with(String::new)
                                .push_str(thinking);

                            if tx
                                .send(Ok(ApiEvent::Thinking(thinking.to_string())))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }

                        // tool_calls delta
                        if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            let mut min_seen_index: Option<usize> = None;
                            for tc in tc_array {
                                let idx =
                                    tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                min_seen_index =
                                    Some(min_seen_index.map_or(idx, |min| min.min(idx)));

                                let entry = tool_calls.entry(idx).or_insert_with(ToolCallAcc::new);

                                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                    entry.id = Some(id.to_string());
                                }

                                if let Some(name) = tc
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|v| v.as_str())
                                {
                                    entry.name = Some(name.to_string());
                                }

                                if let Some(args) = tc
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|v| v.as_str())
                                {
                                    entry.arguments.push_str(args);
                                }
                            }

                            if let Some(min_seen_index) = min_seen_index
                                && min_seen_index > next_expected_tool_index
                            {
                                push_accumulated_blocks(
                                    &mut accumulated_text,
                                    &mut accumulated_reasoning,
                                    &mut content_blocks,
                                );
                                if let Err(err) = emit_tool_calls_before_index(
                                    &mut tool_calls,
                                    &mut next_expected_tool_index,
                                    min_seen_index,
                                    &mut content_blocks,
                                    &tx,
                                )
                                .await
                                {
                                    let _ = tx.send(Err(err)).await;
                                    return;
                                }
                            }
                        }
                    }

                    if current_finish_reason == Some("tool_calls") {
                        push_accumulated_blocks(
                            &mut accumulated_text,
                            &mut accumulated_reasoning,
                            &mut content_blocks,
                        );
                        if let Err(err) = emit_all_pending_tool_calls(
                            &mut tool_calls,
                            &mut next_expected_tool_index,
                            &mut content_blocks,
                            &tx,
                        )
                        .await
                        {
                            let _ = tx.send(Err(err)).await;
                            return;
                        }
                    }
                }

                Ok(Err(err)) => {
                    consecutive_errors += 1;
                    tracing::warn!(msg = "SSE parse error", error = %err, consecutive_errors, max = MAX_CONSECUTIVE_ERRORS);
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        tracing::error!("Too many consecutive SSE errors, stopping stream");
                        let _ = tx.send(Err(StreamError::Sse(err.to_string()))).await;
                        return;
                    }
                    continue;
                }

                Err(_elapsed) => {
                    tracing::warn!("SSE stream timed out after 90s");
                    let _ = tx.send(Err(StreamError::UnexpectedEnd)).await;
                    return;
                }
            }
        }

        if !saw_done && !saw_finish_reason {
            let _ = tx.send(Err(StreamError::UnexpectedEnd)).await;
            return;
        }

        push_accumulated_blocks(
            &mut accumulated_text,
            &mut accumulated_reasoning,
            &mut content_blocks,
        );

        if let Err(err) = emit_all_pending_tool_calls(
            &mut tool_calls,
            &mut next_expected_tool_index,
            &mut content_blocks,
            &tx,
        )
        .await
        {
            let _ = tx.send(Err(err)).await;
            return;
        }

        // 发送 Done
        let completion = ApiCompletion {
            message: Message::new(Role::Assistant, std::mem::take(&mut content_blocks)),
            finish_reason,
            usage: Usage {
                prompt_tokens: prompt_tokens as usize,
                completion_tokens: completion_tokens as usize,
            },
        };
        let _ = tx.send(Ok(ApiEvent::Done(completion))).await;

        tracing::debug!("SSE stream task finished, channel closed");
    });
    Ok(result_stream)
}

#[derive(Clone)]
struct ToolCallAcc {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    emitted: bool,
}

impl ToolCallAcc {
    fn new() -> Self {
        Self {
            id: None,
            name: None,
            arguments: String::new(),
            emitted: false,
        }
    }
}

fn push_accumulated_blocks(
    accumulated_text: &mut Option<String>,
    accumulated_reasoning: &mut Option<String>,
    content_blocks: &mut Vec<ContentBlock>,
) {
    if let Some(text) = accumulated_text.take() {
        content_blocks.push(ContentBlock::from_text(text));
    }

    if let Some(thinking) = accumulated_reasoning.take() {
        content_blocks.push(ContentBlock::from_thinking(thinking));
    }
}

async fn emit_all_pending_tool_calls(
    tool_calls: &mut HashMap<usize, ToolCallAcc>,
    next_expected_index: &mut usize,
    content_blocks: &mut Vec<ContentBlock>,
    tx: &tokio::sync::mpsc::Sender<Result<ApiEvent, StreamError>>,
) -> Result<(), StreamError> {
    let Some(upper_bound) = tool_calls.keys().copied().max().map(|index| index + 1) else {
        return Ok(());
    };

    emit_tool_calls_before_index(
        tool_calls,
        next_expected_index,
        upper_bound,
        content_blocks,
        tx,
    )
    .await
}

/// Emit completed tool calls in index order.
///
/// OpenAI does not send an explicit per-tool stop event. When a higher index starts,
/// lower indexes are complete enough to dispatch, while still preserving order.
async fn emit_tool_calls_before_index(
    tool_calls: &mut HashMap<usize, ToolCallAcc>,
    next_expected_index: &mut usize,
    upper_bound_exclusive: usize,
    content_blocks: &mut Vec<ContentBlock>,
    tx: &tokio::sync::mpsc::Sender<Result<ApiEvent, StreamError>>,
) -> Result<(), StreamError> {
    while *next_expected_index < upper_bound_exclusive {
        let Some(tc) = tool_calls.get_mut(next_expected_index) else {
            return Err(StreamError::UnexpectedEnd);
        };
        if tc.emitted {
            *next_expected_index += 1;
            continue;
        }

        let (Some(id), Some(name)) = (&tc.id, &tc.name) else {
            return Err(StreamError::UnexpectedEnd);
        };
        let input: HashMap<String, Value> = serde_json::from_str(&tc.arguments)?;
        let tool_use = ToolUseBlock {
            id: id.clone(),
            name: name.clone(),
            input,
        };
        content_blocks.push(ContentBlock::ToolUse(tool_use.clone()));
        if tx.send(Ok(ApiEvent::ToolUse(tool_use))).await.is_err() {
            return Err(StreamError::ChannelClosed);
        }
        tc.emitted = true;
        *next_expected_index += 1;
    }

    Ok(())
}

fn convert_messages_to_openai(messages: &[Message], system_prompt: Option<&str>) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();

    // OpenAI 的 system prompt 作为一条 system 消息
    if let Some(ref system) = system_prompt {
        result.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }

    for msg in messages {
        match msg.role {
            Role::User => {
                // Anthropic 风格的 tool_result 内联在 user message 中，
                // OpenAI 需要拆成独立的 tool 消息
                let tool_results: Vec<&ToolResultBlock> = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult(tr) => Some(tr),
                        _ => None,
                    })
                    .collect();

                if !tool_results.is_empty() {
                    for tr in tool_results {
                        result.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tr.tool_use_id,
                            "content": tr.content,
                        }));
                    }
                    continue;
                }

                // 普通 user 消息：提取文本
                let texts: Vec<&str> = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();

                if texts.is_empty() {
                    continue;
                }

                let content = if texts.len() == 1 {
                    Value::String(texts[0].to_string())
                } else {
                    Value::Array(
                        texts
                            .iter()
                            .map(|t| serde_json::json!({"type": "text", "text": t}))
                            .collect(),
                    )
                };

                result.push(serde_json::json!({
                    "role": "user",
                    "content": content,
                }));
            }

            Role::Assistant => {
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                let mut reasoning_content: Option<String> = None;

                for block in &msg.content {
                    match block {
                        ContentBlock::Text(t) => {
                            text_parts.push(t.text.clone());
                        }
                        ContentBlock::ToolUse(tu) => {
                            tool_calls.push(serde_json::json!({
                                "id": tu.id,
                                "type": "function",
                                "function": {
                                    "name": tu.name,
                                    "arguments": serde_json::to_value(&tu.input)
                                        .map(|v| v.to_string())
                                        .unwrap_or_default(),
                                }
                            }));
                        }
                        ContentBlock::Thinking(th) => {
                            // 取第一个 thinking 块作为 reasoning_content
                            if reasoning_content.is_none() {
                                reasoning_content = Some(th.thinking.clone());
                            }
                        }
                        _ => {}
                    }
                }

                let mut assistant = serde_json::Map::new();
                assistant.insert("role".to_string(), Value::String("assistant".to_string()));

                // OpenAI 要求 tool-only assistant 消息 content 为 null
                let content = if text_parts.is_empty() {
                    Value::Null
                } else if text_parts.len() == 1 {
                    Value::String(text_parts.into_iter().next().unwrap())
                } else {
                    Value::String(text_parts.join(""))
                };
                assistant.insert("content".to_string(), content);

                if !tool_calls.is_empty() {
                    assistant.insert("tool_calls".to_string(), Value::Array(tool_calls));
                }

                if let Some(reasoning) = reasoning_content {
                    assistant.insert("reasoning_content".to_string(), Value::String(reasoning));
                }

                result.push(Value::Object(assistant));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCallAcc {
        ToolCallAcc {
            id: Some(id.to_string()),
            name: Some(name.to_string()),
            arguments: arguments.to_string(),
            emitted: false,
        }
    }

    #[tokio::test]
    async fn emits_tool_calls_as_next_index_starts_in_order() {
        let (tx, mut rx) = api_channel(4);
        let mut tool_calls = HashMap::new();
        tool_calls.insert(0, tool_call("call_0", "first", r#"{"a":1}"#));
        tool_calls.insert(1, tool_call("call_1", "second", r#"{"b":2}"#));
        let mut next_expected_index = 0;
        let mut content_blocks = Vec::new();

        emit_tool_calls_before_index(
            &mut tool_calls,
            &mut next_expected_index,
            1,
            &mut content_blocks,
            &tx,
        )
        .await
        .unwrap();

        assert_eq!(next_expected_index, 1);
        assert_eq!(content_blocks.len(), 1);
        match rx.next().await.unwrap().unwrap() {
            ApiEvent::ToolUse(tool_use) => {
                assert_eq!(tool_use.id, "call_0");
                assert_eq!(tool_use.name, "first");
                assert_eq!(tool_use.input.get("a"), Some(&serde_json::json!(1)));
            }
            other => panic!("expected tool use, got {other:?}"),
        }

        emit_all_pending_tool_calls(
            &mut tool_calls,
            &mut next_expected_index,
            &mut content_blocks,
            &tx,
        )
        .await
        .unwrap();

        assert_eq!(next_expected_index, 2);
        assert_eq!(content_blocks.len(), 2);
        match rx.next().await.unwrap().unwrap() {
            ApiEvent::ToolUse(tool_use) => {
                assert_eq!(tool_use.id, "call_1");
                assert_eq!(tool_use.name, "second");
                assert_eq!(tool_use.input.get("b"), Some(&serde_json::json!(2)));
            }
            other => panic!("expected tool use, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_tool_arguments_return_json_error() {
        let (tx, _rx) = api_channel(4);
        let mut tool_calls = HashMap::new();
        tool_calls.insert(0, tool_call("call_0", "broken", r#"{"a":"#));
        let mut next_expected_index = 0;
        let mut content_blocks = Vec::new();

        let err = emit_all_pending_tool_calls(
            &mut tool_calls,
            &mut next_expected_index,
            &mut content_blocks,
            &tx,
        )
        .await
        .unwrap_err();

        assert!(matches!(err, StreamError::Json(_)));
        assert!(content_blocks.is_empty());
        assert_eq!(next_expected_index, 0);
    }
}
