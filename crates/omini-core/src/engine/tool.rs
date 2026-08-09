use crate::tools::{
    PendingToolPauses, ToolExecutionContext, ToolRegistry, ToolResult, ToolRuntimeContext,
};
use crate::types::events::EngineToRuntimeEvent;
use omini_config::Settings;
use omini_domain::events::ActiveProfile;
use omini_domain::message::{ContentBlock, Message, Role, ToolResultBlock, ToolUseBlock};
use omini_permissions::PermissionEngine;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::{Notify, mpsc};
use tokio::task::{JoinError, JoinSet};

const LOG_SUMMARY_MAX_CHARS: usize = 2048;

#[derive(Debug, Clone)]
pub struct ToolRunResult {
    pub(super) block: ToolResultBlock,
    extra_blocks: Option<Vec<ContentBlock>>,
}

impl ToolRunResult {
    pub fn new(block: ToolResultBlock, extra_blocks: Option<Vec<ContentBlock>>) -> Self {
        Self {
            block,
            extra_blocks,
        }
    }
}

pub enum ToolDrain {
    Complete(Vec<ToolRunResult>),
    Cancelled(Vec<ToolRunResult>),
}

#[derive(Clone)]
pub struct ToolExecutor {
    settings: Arc<Settings>,
    pending_tool_pauses: PendingToolPauses,
    permission_engine: Arc<PermissionEngine>,
    active_profile: Arc<RwLock<ActiveProfile>>,
    cancelled: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    runtime_context: Option<Arc<ToolRuntimeContext>>,
    tool_registry: Arc<ToolRegistry>,
    event_tx: mpsc::Sender<EngineToRuntimeEvent>,
}

impl ToolExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        settings: Arc<Settings>,
        pending_tool_pauses: PendingToolPauses,
        permission_engine: Arc<PermissionEngine>,
        active_profile: Arc<RwLock<ActiveProfile>>,
        cancelled: Arc<AtomicBool>,
        cancel_notify: Arc<Notify>,
        runtime_context: Option<Arc<ToolRuntimeContext>>,
        tool_registry: Arc<ToolRegistry>,
        event_tx: mpsc::Sender<EngineToRuntimeEvent>,
    ) -> Self {
        Self {
            settings,
            pending_tool_pauses,
            permission_engine,
            active_profile,
            cancelled,
            cancel_notify,
            runtime_context,
            tool_registry,
            event_tx,
        }
    }

    pub async fn execute(self, tool_use: ToolUseBlock) -> ToolRunResult {
        if self.cancelled.load(Ordering::Relaxed) {
            return ToolRunResult::new(cancelled_result(&tool_use.id), None);
        }

        tracing::debug!(
            tool_use_id = %tool_use.id,
            tool_name = %tool_use.name,
            tool_input = %summarize_input(&tool_use.input),
            "tool execution started"
        );

        let result = if let Some(tool) = self.tool_registry.get(&tool_use.name) {
            let active_profile = *self
                .active_profile
                .read()
                .expect("active profile lock poisoned");
            let pause_id = self
                .runtime_context
                .as_ref()
                .filter(|runtime| runtime.agent_depth > 0)
                .map(|runtime| format!("{}:{}", runtime.thread_id, tool_use.id))
                .unwrap_or_else(|| tool_use.id.clone());

            let context = ToolExecutionContext {
                tool_use_id: tool_use.id.clone(),
                pause_id,
                tool_name: tool_use.name.clone(),
                settings: self.settings,
                tool_registry: self.tool_registry.clone(),
                event_tx: self.event_tx.clone(),
                pending_tool_pauses: self.pending_tool_pauses,
                permission_engine: self.permission_engine,
                active_profile,
                cancelled: self.cancelled,
                cancel_notify: self.cancel_notify,
                runtime: self.runtime_context,
            };
            tool.execute(tool_use.input.clone(), context).await
        } else {
            ToolResult::error(format!("Unknown tool: {}", tool_use.name))
        };

        let (block, extra_blocks) = result.into_parts(&tool_use.id);
        tracing::debug!(
            tool_use_id = %block.tool_use_id,
            tool_name = %tool_use.name,
            is_error = block.is_error,
            output_summary = %summarize_output(&block.content),
            "tool execution finished"
        );
        let _ = self
            .event_tx
            .send(EngineToRuntimeEvent::ToolResult(block.clone()))
            .await;
        ToolRunResult::new(block, extra_blocks)
    }
}

/// 等待当前 Turn 已启动的工具任务。`ready` 保存未启动任务便直接生成的结果。
pub async fn drain_tasks(
    tasks: &mut JoinSet<ToolRunResult>,
    mut ready: Vec<ToolRunResult>,
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    cancelled: &AtomicBool,
    cancel_notify: &Notify,
) -> ToolDrain {
    while !tasks.is_empty() {
        if cancelled.load(Ordering::Relaxed) {
            tasks.abort_all();
        }

        let joined = tokio::select! {
            joined = tasks.join_next() => joined,
            _ = cancel_notify.notified() => {
                if cancelled.load(Ordering::Relaxed) {
                    tasks.abort_all();
                }
                continue;
            }
        };

        let Some(joined) = joined else {
            break;
        };
        collect_join(joined, &mut ready, event_tx).await;
    }

    if cancelled.load(Ordering::Relaxed) {
        ToolDrain::Cancelled(ready)
    } else {
        ToolDrain::Complete(ready)
    }
}

/// 终止并排空当前 Turn 的工具任务，返回终止前已经完成的结果。
pub async fn abort_tasks(
    tasks: &mut JoinSet<ToolRunResult>,
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
) -> Vec<ToolRunResult> {
    tasks.abort_all();
    let mut completed = Vec::new();

    while let Some(joined) = tasks.join_next().await {
        collect_join(joined, &mut completed, event_tx).await;
    }
    completed
}

async fn collect_join(
    joined: Result<ToolRunResult, JoinError>,
    output: &mut Vec<ToolRunResult>,
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
) {
    match joined {
        Ok(result) => output.push(result),
        Err(error) if error.is_cancelled() => {
            tracing::debug!("tool task cancelled");
        }
        Err(error) => {
            tracing::error!(error = %error, "tool task panicked");
            let _ = event_tx
                .send(EngineToRuntimeEvent::Error(format!(
                    "Tool task panicked: {error}"
                )))
                .await;
        }
    }
}

/// 按 assistant 中的 tool_use 顺序整理结果，并为没有完成的调用补取消结果。
pub async fn reconcile_cancelled(
    assistant: &Message,
    completed: Vec<ToolRunResult>,
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
) -> Vec<ToolRunResult> {
    let mut by_id = completed
        .into_iter()
        .map(|result| (result.block.tool_use_id.clone(), result))
        .collect::<HashMap<_, _>>();

    let mut results = Vec::new();
    for block in &assistant.content {
        let ContentBlock::ToolUse(tool_use) = block else {
            continue;
        };

        if let Some(result) = by_id.remove(&tool_use.id) {
            results.push(result);
        } else {
            let block = cancelled_result(&tool_use.id);
            let _ = event_tx
                .send(EngineToRuntimeEvent::ToolResult(block.clone()))
                .await;
            results.push(ToolRunResult::new(block, None));
        }
    }
    results
}

/// 将并发完成顺序恢复为 assistant 中的 tool_use 顺序。
pub fn align_results(assistant: &Message, results: Vec<ToolRunResult>) -> Vec<ToolRunResult> {
    let mut by_id = results
        .into_iter()
        .map(|result| (result.block.tool_use_id.clone(), result))
        .collect::<HashMap<_, _>>();

    assistant
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse(tool_use) => by_id.remove(&tool_use.id),
            _ => None,
        })
        .collect()
}

/// 将工具结果提交到 LLM 历史并发送对应 Runtime 事件。
pub async fn commit_results(
    messages: &mut Vec<Message>,
    results: Vec<ToolRunResult>,
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
) {
    if results.is_empty() {
        return;
    }

    let (llm_message, display_message, has_extra_blocks) = build_messages(results);
    if has_extra_blocks {
        let _ = event_tx
            .send(EngineToRuntimeEvent::LlmHistoryProduced(
                llm_message.clone(),
            ))
            .await;
        let _ = event_tx
            .send(EngineToRuntimeEvent::ToolResultsDisplayProduced(
                display_message,
            ))
            .await;
    } else {
        let _ = event_tx
            .send(EngineToRuntimeEvent::ToolResultsProduced(
                llm_message.clone(),
            ))
            .await;
    }
    messages.push(llm_message);
}

pub fn build_messages(results: Vec<ToolRunResult>) -> (Message, Message, bool) {
    let mut display_blocks = Vec::new();
    let mut llm_blocks = Vec::new();
    let mut has_extra_blocks = false;

    for result in results {
        display_blocks.push(ContentBlock::ToolResult(result.block.clone()));
        llm_blocks.push(ContentBlock::ToolResult(result.block));

        if let Some(extra_blocks) = result.extra_blocks
            && !extra_blocks.is_empty()
        {
            has_extra_blocks = true;
            llm_blocks.extend(extra_blocks);
        }
    }

    (
        Message::new(Role::User, llm_blocks),
        Message::new(Role::User, display_blocks),
        has_extra_blocks,
    )
}

fn cancelled_result(tool_use_id: &str) -> ToolResultBlock {
    ToolResultBlock {
        tool_use_id: tool_use_id.to_string(),
        is_error: true,
        content: "Execution cancelled".to_string(),
        metadata: None,
    }
}

fn summarize_input(input: &HashMap<String, serde_json::Value>) -> String {
    let value = serde_json::to_string(input).unwrap_or_else(|_| "<invalid json>".to_string());
    summarize(&value, LOG_SUMMARY_MAX_CHARS)
}

fn summarize_output(output: &str) -> String {
    summarize(output, LOG_SUMMARY_MAX_CHARS)
}

fn summarize(value: &str, max_chars: usize) -> String {
    let mut iter = value.chars();
    let summary = iter.by_ref().take(max_chars).collect::<String>();
    if iter.next().is_some() {
        format!("{summary}...[truncated]")
    } else {
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_domain::message::ContentBlock;
    use tokio::sync::mpsc;

    #[test]
    fn tool_result_messages_keep_extra_blocks_only_in_llm_message() {
        let result = ToolRunResult::new(
            ToolResultBlock {
                tool_use_id: "toolu_image".to_string(),
                is_error: false,
                content: "Loaded image".to_string(),
                metadata: None,
            },
            Some(vec![ContentBlock::from_base64_image(
                "image/png".to_string(),
                "abc123".to_string(),
            )]),
        );

        let (llm_message, display_message, has_extra_blocks) = build_messages(vec![result]);

        assert!(has_extra_blocks);
        assert_eq!(llm_message.content.len(), 2);
        assert!(matches!(
            llm_message.content[0],
            ContentBlock::ToolResult(_)
        ));
        assert!(matches!(llm_message.content[1], ContentBlock::Image(_)));
        assert_eq!(display_message.content.len(), 1);
        assert!(matches!(
            display_message.content[0],
            ContentBlock::ToolResult(_)
        ));
    }

    #[test]
    fn extra_blocks_only_enter_llm_history() {
        let result = ToolRunResult::new(
            ToolResultBlock {
                tool_use_id: "toolu_image".to_string(),
                is_error: false,
                content: "Loaded image".to_string(),
                metadata: None,
            },
            Some(vec![ContentBlock::from_base64_image(
                "image/png".to_string(),
                "abc123".to_string(),
            )]),
        );

        let (llm_message, display_message, has_extra_blocks) = build_messages(vec![result]);

        assert!(has_extra_blocks);
        assert_eq!(llm_message.content.len(), 2);
        assert_eq!(display_message.content.len(), 1);
    }

    #[tokio::test]
    async fn commit_results_ignores_empty_batches() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut messages = Vec::new();

        commit_results(&mut messages, Vec::new(), &tx).await;

        assert!(messages.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn tool_execution_reads_profile_when_the_tool_starts() {
        let context = ToolExecutionContext::test("write");
        let active_profile = Arc::new(RwLock::new(ActiveProfile::Main));
        let executor = ToolExecutor::new(
            Arc::clone(&context.settings),
            Arc::clone(&context.pending_tool_pauses),
            Arc::clone(&context.permission_engine),
            Arc::clone(&active_profile),
            Arc::clone(&context.cancelled),
            Arc::clone(&context.cancel_notify),
            None,
            Arc::new(crate::tools::create_main_registry()),
            context.event_tx.clone(),
        );
        *active_profile
            .write()
            .expect("active profile lock poisoned") = ActiveProfile::Plan;

        let result = executor
            .execute(ToolUseBlock {
                id: "tool_1".to_string(),
                name: "write".to_string(),
                input: HashMap::new(),
            })
            .await;

        assert!(result.block.is_error);
        assert!(
            result
                .block
                .content
                .contains("not available in plan profile")
        );
    }
}
