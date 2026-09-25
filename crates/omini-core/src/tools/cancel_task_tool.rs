use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CancelTaskInput {
    /// Task ID returned by `spawn_agent`.
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
        "Idempotently cancel one agent task and all of its descendants without affecting sibling tasks. Only the main agent can use this tool."
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
        let Some(supervisor) = &runtime.task_supervisor else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.cancel_task(&task_id).await
    }
}
