use crate::api::{ApiRequest, FinishReason, LlmClient};
use crate::types::config::Settings;
use crate::types::events::RuntimeEvent;
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
/// `messages` 是可变引用，以便 `run_query` 直接将 assistant 回复和 tool_result 追加进去。
#[derive(Debug)]
pub struct QueryContext<'a> {
    pub messages: &'a mut Vec<Message>,
    pub settings: &'a Settings,
    pub llm_client: &'a LlmClient,
}

/// 查询引擎。
///
/// 有状态 —— 未来会持有：
/// - `permission_system`: 工具调用前的权限检查
/// - `hook_system`:  LLM 调用前后 / 工具执行前后的钩子
///
/// 当前只负责：
/// 1. 从 `QueryContext` 构建 `ApiRequest`
/// 2. 调用 `LlmClient` 发起流式请求
/// 3. 将 `ApiEvent` 转发为 `RuntimeEvent` 给 UI
/// 4. 收集 tool_use → 执行工具 → 追加 tool_result 到 messages
/// 5. 多轮循环直到 LLM 自然结束或达到最大轮次
pub struct QueryEngine {
    // ── 未来字段 ──
    // permission_system: PermissionSystem,
    // hook_system: HookSystem,
}

impl QueryEngine {
    /// 创建新的查询引擎。
    pub fn new() -> Self {
        Self {
            // permission_system: PermissionSystem::new(),
            // hook_system: HookSystem::new(),
        }
    }

    /// 执行一次完整的查询（可能包含多轮 LLM 调用 + 工具执行）。
    ///
    /// # 参数
    /// - `ctx`: 查询上下文（messages、settings、llm_client）
    /// - `event_tx`: 向 UI 发送事件的 channel
    /// - `cancelled`: 取消标志（来自 `AgentRuntime`）
    ///
    /// # 返回
    /// `QueryResult` 包含执行摘要。
    pub async fn run_query(
        &self,
        ctx: QueryContext<'_>,
        event_tx: mpsc::Sender<RuntimeEvent>,
        cancelled: Arc<AtomicBool>,
    ) -> QueryResult {
        let max_turns = ctx.settings.max_turns.unwrap_or(200);
        let mut turns = 0;
        let mut finish_reason = FinishReason::Stop;
        let mut had_tool_use = false;
        // 在整个 query 生命周期内复用同一个 JoinSet，每轮 drain 后自动清空
        let mut tool_tasks: JoinSet<ToolResultBlock> = JoinSet::new();

        for _turn in 0..max_turns {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }

            let _ = event_tx.send(RuntimeEvent::TurnStarted).await;

            let request = ApiRequest {
                messages: ctx.messages,
                model: &ctx.settings.model,
                system_prompt: ctx.settings.system_prompt.as_deref(),
                max_tokens: None,
                temperature: None,
            };

            let mut stream = match ctx.llm_client.invoke(request).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx
                        .send(RuntimeEvent::Error(format!("LLM request failed: {e}")))
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
                            let _ = event_tx.send(RuntimeEvent::TextDelta(delta)).await;
                        }
                        crate::api::ApiEvent::Thinking(delta) => {
                            let _ = event_tx.send(RuntimeEvent::ThinkingDelta(delta)).await;
                        }
                        crate::api::ApiEvent::ToolUse(tool_use) => {
                            // 通知 UI：tool 开始执行
                            let _ = event_tx.send(RuntimeEvent::ToolUse(tool_use.clone())).await;

                            // 立即在后台 spawn 执行 tool
                            let tx = event_tx.clone();
                            let cancelled = cancelled.clone();
                            tool_tasks.spawn(async move {
                                Self::execute_tool(&tool_use, &tx, cancelled).await
                            });
                        }
                        crate::api::ApiEvent::Done(completion) => {
                            stream_completion = Some(completion);
                        }
                    },
                    Err(stream_err) => {
                        let _ = event_tx
                            .send(RuntimeEvent::Error(format!("Stream error: {stream_err}")))
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
                            .send(RuntimeEvent::Error("Stream ended unexpectedly".into()))
                            .await;
                    }
                    break;
                }
            };

            // TODO: 需要将token信息同步(占位)
            finish_reason = completion.finish_reason.clone();

            ctx.messages.push(completion.message);

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
                            .send(RuntimeEvent::Error(format!(
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
                ctx.messages.push(Message::new(Role::User, result_blocks));
            }

            let _ = event_tx.send(RuntimeEvent::TurnEnded).await;
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
    /// TODO:
    /// - 集成权限系统（执行前发送 `PermissionRequest`）
    /// - 集成钩子系统（执行前后钩子）
    /// - 根据 `tool_use.name` 分发到具体工具实现
    async fn execute_tool(
        tool_use: &ToolUseBlock,
        event_tx: &mpsc::Sender<RuntimeEvent>,
        _cancelled: Arc<AtomicBool>,
    ) -> ToolResultBlock {
        // TODO: 实际的 tool 分发 + 执行逻辑
        // 暂时返回空结果占位
        let result = ToolResultBlock {
            tool_use_id: tool_use.id.clone(),
            is_error: false,
            // TODO: 等待实际的 tool 执行后填充真实输出
            output: String::new(),
        };
        let _ = event_tx
            .send(RuntimeEvent::ToolResult(result.clone()))
            .await;
        result
    }
}

impl Default for QueryEngine {
    fn default() -> Self {
        Self::new()
    }
}
