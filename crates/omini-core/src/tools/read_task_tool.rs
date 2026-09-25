use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadTaskInput {
    /// Task ID returned by `spawn_agent`.
    pub task_id: String,
}

pub struct ReadTaskTool;

#[async_trait]
impl Tool for ReadTaskTool {
    type Input = ReadTaskInput;

    fn name(&self) -> &str {
        "read_task"
    }

    fn description(&self) -> &str {
        "Read an agent task's current status and optional terminal output/error/warnings. Usually rely on automatic completion notifications; use this only when you need to inspect a task without waiting. Only the main agent can use this tool."
    }

    async fn call(&self, input: Self::Input, ctx: ToolExecutionContext) -> ToolResult {
        let task_id = match super::task_support::normalize_task_id(&input.task_id) {
            Ok(task_id) => task_id,
            Err(result) => return result,
        };
        let Some(runtime) = ctx.runtime else {
            return ToolResult::error("read_task requires runtime context");
        };
        if runtime.agent_depth != 0 {
            return ToolResult::error("read_task is only available to the main agent");
        }
        let Some(supervisor) = &runtime.task_supervisor else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.read_task(&task_id)
    }
}
