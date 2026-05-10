use crate::api::{ApiRequest, FinishReason, LlmClient};
use crate::tools::{ToolRegistry, ToolResult};
use crate::types::config::Settings;
use crate::types::events::EngineEvent;
use crate::types::message::{ContentBlock, Message, Role, ToolResultBlock, ToolUseBlock};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_stream::StreamExt;

/// 一次查询执行后的结果摘要。
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// 本轮查询总共生成的 assistant 消息数（= 轮次）
    pub turns: usize,
    /// 最终的 finish_reason（最后一轮 LLM 返回的原因）
    pub finish_reason: FinishReason,
    /// 本轮是否有工具调用被执行
    pub had_tool_use: bool,
}

/// 一次查询的上下文。
///
/// 由 `AgentRuntime` 在每次 `process_run` 时构造传入。
/// `messages` 是引擎本地的工作副本，runtime 通过 `EngineEvent` 获取新消息。
#[derive(Debug)]
pub struct QueryContext<'a> {
    pub messages: &'a mut Vec<Message>,
    pub settings: &'a Settings,
    pub llm_client: &'a LlmClient,
    pub tool_registry: Arc<ToolRegistry>,
}

/// 查询引擎。
pub struct QueryEngine;

impl QueryEngine {
    /// 创建新的查询引擎。
    pub fn new() -> Self {
        Self
    }

    /// 执行一次完整的查询（可能包含多轮 LLM 调用 + 工具执行）。
    ///
    /// # 参数
    /// - `ctx`: 查询上下文（messages、settings、llm_client）
    /// - `event_tx`: 向 Runtime 发送事件的 channel
    /// - `cancelled`: 取消标志（来自 `AgentRuntime`）
    ///
    /// # 返回
    /// `QueryResult` 包含执行摘要。
    pub async fn run_query(
        &self,
        ctx: QueryContext<'_>,
        event_tx: mpsc::Sender<EngineEvent>,
        cancelled: Arc<AtomicBool>,
    ) -> QueryResult {
        let max_turns = ctx.settings.max_turns.unwrap_or(200);
        let mut turns = 0;
        let mut finish_reason = FinishReason::Stop;
        let mut had_tool_use = false;
        // 在整个 query 生命周期内复用同一个 JoinSet，每轮 drain 后自动清空
        let mut tool_tasks: JoinSet<ToolResultBlock> = JoinSet::new();
        // 工具定义在一次 process_run 中不会变化，预先计算一次避免重复 clone
        let tool_definitions = ctx.tool_registry.definitions();

        for _turn in 0..max_turns {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }

            let _ = event_tx.send(EngineEvent::TurnStarted).await;

            let request = ApiRequest {
                messages: ctx.messages,
                model: &ctx.settings.model,
                system_prompt: ctx.settings.system_prompt.as_deref(),
                tools: Some(&tool_definitions),
                max_tokens: None,
                temperature: None,
                thinking_effort: ctx.settings.thinking_effort,
            };

            let mut stream = match ctx.llm_client.invoke(request).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx
                        .send(EngineEvent::Error(format!("LLM request failed: {e}")))
                        .await;
                    break;
                }
            };

            let mut stream_completion = None;

            while let Some(event) = stream.next().await {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }

                match event {
                    Ok(api_event) => match api_event {
                        crate::api::ApiEvent::Text(delta) => {
                            let _ = event_tx.send(EngineEvent::TextDelta(delta)).await;
                        }
                        crate::api::ApiEvent::Thinking(delta) => {
                            let _ = event_tx.send(EngineEvent::ThinkingDelta(delta)).await;
                        }
                        crate::api::ApiEvent::ToolUse(tool_use) => {
                            // 通知 UI：tool 开始执行
                            let _ = event_tx.send(EngineEvent::ToolUse(tool_use.clone())).await;

                            // 立即在后台 spawn 执行 tool
                            let tx = event_tx.clone();
                            let cancelled = cancelled.clone();
                            let tool_registry = ctx.tool_registry.clone();
                            tool_tasks.spawn(async move {
                                Self::execute_tool(&tool_registry, &tool_use, &tx, cancelled).await
                            });
                        }
                        crate::api::ApiEvent::Done(completion) => {
                            stream_completion = Some(completion);
                        }
                    },
                    Err(stream_err) => {
                        let _ = event_tx
                            .send(EngineEvent::Error(format!("Stream error: {stream_err}")))
                            .await;
                        break;
                    }
                }
            }

            // 检查 stream 是否正常结束
            let completion = match stream_completion {
                Some(c) => c,
                None => {
                    if !cancelled.load(Ordering::Relaxed) {
                        let _ = event_tx
                            .send(EngineEvent::Error("Stream ended unexpectedly".into()))
                            .await;
                    }
                    break;
                }
            };

            // TODO: 需要将token信息同步(占位)
            finish_reason = completion.finish_reason.clone();

            let msg = completion.message;
            let _ = event_tx
                .send(EngineEvent::MessageProduced(msg.clone()))
                .await;
            ctx.messages.push(msg);

            // JoinSet 按完成顺序返回，但 tool_result 需要与 assistant 消息中的 tool_use 顺序一致
            // 因此用 tool_use_id 建立查找表来重建顺序
            let mut tool_results: Vec<ToolResultBlock> = Vec::new();
            while let Some(task_result) = tool_tasks.join_next().await {
                match task_result {
                    Ok(tool_result) => {
                        tool_results.push(tool_result);
                    }
                    Err(join_err) => {
                        let _ = event_tx
                            .send(EngineEvent::Error(format!(
                                "Tool task panicked: {join_err}"
                            )))
                            .await;
                    }
                }
            }

            if !tool_results.is_empty() {
                had_tool_use = true;
                // 按 assistant 消息中 tool_use 的顺序重排 tool_result
                let mut tool_result_map: std::collections::HashMap<String, ToolResultBlock> =
                    tool_results
                        .into_iter()
                        .map(|r| (r.tool_use_id.clone(), r))
                        .collect();
                // 从刚刚 push 的 assistant 消息中获取 tool_use 的原始顺序
                let result_blocks: Vec<ContentBlock> = ctx
                    .messages
                    .last()
                    .expect("assistant message just pushed")
                    .content
                    .iter()
                    .filter_map(|block| {
                        if let ContentBlock::ToolUse(tu) = block {
                            tool_result_map.remove(&tu.id).map(ContentBlock::ToolResult)
                        } else {
                            None
                        }
                    })
                    .collect();
                let tool_msg = Message::new(Role::User, result_blocks);
                let _ = event_tx
                    .send(EngineEvent::ToolResultsProduced(tool_msg.clone()))
                    .await;
                ctx.messages.push(tool_msg);
            }

            let _ = event_tx.send(EngineEvent::TurnEnded).await;
            turns += 1;

            // 如果 LLM 没有请求 tool 调用，结束循环
            if !matches!(finish_reason, FinishReason::ToolUse) {
                break;
            }
        }

        QueryResult {
            turns,
            finish_reason,
            had_tool_use,
        }
    }

    /// 执行单个工具调用。
    ///
    async fn execute_tool(
        tool_registry: &ToolRegistry,
        tool_use: &ToolUseBlock,
        event_tx: &mpsc::Sender<EngineEvent>,
        cancelled: Arc<AtomicBool>,
    ) -> ToolResultBlock {
        if cancelled.load(Ordering::Relaxed) {
            return ToolResultBlock {
                tool_use_id: tool_use.id.clone(),
                is_error: true,
                content: "Execution cancelled".into(),
            };
        }

        let result = if let Some(tool) = tool_registry.get(&tool_use.name) {
            tool.execute(tool_use.input.clone()).await
        } else {
            ToolResult::error(format!("Unknown tool: {}", tool_use.name))
        };

        let block = result.into_block(&tool_use.id);
        let _ = event_tx.send(EngineEvent::ToolResult(block.clone())).await;
        block
    }
}

impl Default for QueryEngine {
    fn default() -> Self {
        Self::new()
    }
}
