use crate::api::{
    ApiCompletion, ApiEvent, ApiRequest, ApiStream, FinishReason, RequestError, Usage, api_channel,
    send_with_retry, sse::IntoSseStream,
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

    // 当启用思考时，确保 max_tokens 足够大（thinking tokens 也消耗 max_tokens）
    let max_tokens = request.max_tokens.unwrap_or(
        if request
            .thinking_effort
            .is_some_and(|e| e != ThinkingEffort::None)
        {
            16384
        } else {
            8192
        },
    );
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
        // 当前正在累积的 tool_call index（用于检测 index 切换）
        let mut active_tool_call_index: Option<usize> = None;
        // 下一个期望发送的 tool_call index，用于保证顺序
        let mut next_expected_index: usize = 0;
        // 最终组装
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

                    // OpenAI 用 [DONE] 结尾
                    if sse_event.data == "[DONE]" {
                        break 'stream;
                    }

                    if sse_event.data.is_empty() {
                        continue;
                    }

                    let data: Value = match serde_json::from_str(&sse_event.data) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(msg = "Failed to parse SSE data", error = %e);
                            continue;
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

                    let delta = match choice.get("delta") {
                        Some(d) => d,
                        None => continue,
                    };

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
                            break 'stream;
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
                            break 'stream;
                        }
                    }

                    // tool_calls delta
                    if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        // 收集这一批 chunk 中的所有 index
                        let mut seen_indices: Vec<usize> = Vec::new();

                        for tc in tc_array {
                            let idx =
                                tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            seen_indices.push(idx);

                            let entry = tool_calls.entry(idx).or_insert_with(|| ToolCallAcc {
                                id: None,
                                name: None,
                                arguments: String::new(),
                                emitted: false,
                            });

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

                        // 检测 index 切换：当活跃的 tool_call 不再出现在当前 chunk 中时，
                        // 说明前一个 tool_call 已结束，发送它
                        if let Some(active) = active_tool_call_index
                            && !seen_indices.contains(&active)
                            && let Some(tc) = tool_calls.remove(&active)
                            && tc.id.is_some()
                        {
                            send_tool_use(tc, &mut content_blocks, &tx).await;
                        }

                        // 更新活跃 index 为这批中的最后一个
                        active_tool_call_index = seen_indices.last().copied();
                    }

                    // 当 finish_reason 为 tool_calls 时，按序发送所有 tool_use
                    if current_finish_reason == Some("tool_calls") {
                        flush_ready_tool_calls(
                            &mut tool_calls,
                            &mut next_expected_index,
                            &mut content_blocks,
                            &tx,
                        )
                        .await;
                    }
                }

                Ok(Err(err)) => {
                    consecutive_errors += 1;
                    tracing::warn!(msg = "SSE parse error", error = %err, consecutive_errors, max = MAX_CONSECUTIVE_ERRORS);
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        tracing::error!("Too many consecutive SSE errors, stopping stream");
                        break 'stream;
                    }
                    continue;
                }

                Err(_elapsed) => {
                    tracing::warn!("SSE stream timed out after 90s");
                    break 'stream;
                }
            }
        }

        // 关闭文本块
        if let Some(text) = accumulated_text.take() {
            content_blocks.push(ContentBlock::from_text(text));
        }

        // 关闭思考块
        if let Some(thinking) = accumulated_reasoning.take() {
            content_blocks.push(ContentBlock::from_thinking(thinking));
        }

        // 流结束，发送所有剩余的 tool_calls
        flush_ready_tool_calls(
            &mut tool_calls,
            &mut next_expected_index,
            &mut content_blocks,
            &tx,
        )
        .await;

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

/// 发送单个 tool_call 的 ApiEvent::ToolUse
async fn send_tool_use(
    tc: ToolCallAcc,
    content_blocks: &mut Vec<ContentBlock>,
    tx: &tokio::sync::mpsc::Sender<Result<ApiEvent, crate::api::StreamError>>,
) {
    let input: HashMap<String, Value> = serde_json::from_str(&tc.arguments).unwrap_or_default();
    let tool_use = ToolUseBlock {
        id: tc.id.unwrap_or_default(),
        name: tc.name.unwrap_or_default(),
        input,
    };
    content_blocks.push(ContentBlock::ToolUse(tool_use.clone()));
    let _ = tx.send(Ok(ApiEvent::ToolUse(tool_use))).await;
}

/// 将所有剩余的 tool_call 发送（用于 finish_reason 或流结束时）
/// 按照 index 顺序发送所有就绪的 tool_call。
///
/// 从 `next_expected_index` 开始连续扫描，遇到已初始化且未发出的就发送，
/// 遇到空缺（数据还没到、或未初始化）就停止等待，确保顺序不乱。
async fn flush_ready_tool_calls(
    tool_calls: &mut HashMap<usize, ToolCallAcc>,
    next_expected: &mut usize,
    content_blocks: &mut Vec<ContentBlock>,
    tx: &tokio::sync::mpsc::Sender<Result<ApiEvent, crate::api::StreamError>>,
) {
    loop {
        match tool_calls.get_mut(next_expected) {
            Some(tc) if !tc.emitted && tc.id.is_some() => {
                tc.emitted = true;
                let input: HashMap<String, Value> =
                    serde_json::from_str(&tc.arguments).unwrap_or_default();
                let tool_use = ToolUseBlock {
                    id: tc.id.clone().unwrap_or_default(),
                    name: tc.name.clone().unwrap_or_default(),
                    input,
                };
                content_blocks.push(ContentBlock::ToolUse(tool_use.clone()));
                if tx.send(Ok(ApiEvent::ToolUse(tool_use))).await.is_err() {
                    break;
                }
                *next_expected += 1;
            }
            _ => break,
        }
    }
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
