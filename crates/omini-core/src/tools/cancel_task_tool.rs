use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use omini_domain::task::TaskKind;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CancelTaskInput {
    /// Background task ID.
    pub task_id: String,
}

pub struct CancelTaskTool;

#[async_trait]
impl Tool for CancelTaskTool {
    type Input = CancelTaskInput;

    fn name(&self) -> &str {
        "cancel_task"
    }

    fn description(&self) -> &str {
        "Idempotently cancel one background task and its descendants without affecting sibling tasks. Only the main agent can use this tool."
    }

    async fn call(&self, input: Self::Input, ctx: ToolExecutionContext) -> ToolResult {
        let task_id = match super::task_support::normalize_task_id(&input.task_id) {
            Ok(task_id) => task_id,
            Err(result) => return result,
        };
        let Some(runtime) = ctx.runtime else {
            return ToolResult::error("cancel_task requires runtime context");
        };
        if runtime.agent_depth != 0 {
            return ToolResult::error("cancel_task is only available to the main agent");
        }
        let Some(manager) = &runtime.task_manager else {
            return ToolResult::error("task manager is not available");
        };
        let supervisor = runtime.task_supervisor.as_ref();
        match manager.get(&task_id) {
            Some(task) if task.owner_thread_id != runtime.owner_thread_id => {
                ToolResult::error(format!("unknown task '{task_id}'"))
            }
            Some(task) if task.kind == TaskKind::Bash => match manager.cancel(&task_id).await {
                Ok(task) => ToolResult::ok(
                    serde_json::to_string(&task).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(error) => ToolResult::error(error),
            },
            _ => match supervisor {
                Some(supervisor) => supervisor.cancel_task(&task_id).await,
                None => ToolResult::error("agent task supervisor is not available"),
            },
        }
    }
}
