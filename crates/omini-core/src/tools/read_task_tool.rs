use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use omini_domain::task::TaskKind;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadTaskInput {
    /// Task ID returned when a background task starts.
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
        "Read a background task's current status and optional terminal result. Usually rely on automatic completion notifications; use this only when you need to inspect a task without waiting. Only the main agent can use this tool."
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
        let Some(manager) = &runtime.task_manager else {
            return ToolResult::error("task manager is not available");
        };
        let supervisor = runtime.task_supervisor.as_ref();
        let Some(task) = manager.get(&task_id) else {
            return supervisor.map_or_else(
                || ToolResult::error(format!("unknown task '{task_id}'")),
                |supervisor| supervisor.read_task(&task_id),
            );
        };
        if task.owner_thread_id != runtime.owner_thread_id {
            return ToolResult::error(format!("unknown task '{task_id}'"));
        }
        if task.kind == TaskKind::SubAgent {
            supervisor.map_or_else(
                || ToolResult::error("agent task supervisor is not available"),
                |supervisor| supervisor.read_task(&task_id),
            )
        } else {
            ToolResult::ok(serde_json::to_string(&task).unwrap_or_else(|_| "{}".to_string()))
        }
    }
}
