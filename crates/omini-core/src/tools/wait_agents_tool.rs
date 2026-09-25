use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitAgentsInput {
    /// Task IDs to wait for. Waits for all listed tasks; omit this field to wait for every
    /// currently running background task spawned by the main agent.
    #[serde(default)]
    pub task_ids: Option<Vec<String>>,
}

pub struct WaitAgentsTool;

pub(super) fn normalize_wait_input(
    input: WaitAgentsInput,
) -> Result<Option<Vec<String>>, ToolResult> {
    let Some(task_ids) = input.task_ids else {
        return Ok(None);
    };
    if task_ids.is_empty() {
        return Err(ToolResult::error(
            "task_ids must not be empty when provided",
        ));
    }
    let mut normalized = Vec::with_capacity(task_ids.len());
    for task_id in task_ids {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return Err(ToolResult::error("task_ids must not contain empty IDs"));
        }
        if !normalized.iter().any(|existing| existing == task_id) {
            normalized.push(task_id.to_string());
        }
    }
    Ok(Some(normalized))
}

#[async_trait]
impl Tool for WaitAgentsTool {
    type Input = WaitAgentsInput;

    fn name(&self) -> &str {
        "wait_agents"
    }

    fn description(&self) -> &str {
        "Wait for all listed agent tasks to finish and return their terminal results. If task_ids is omitted, wait for all currently running tasks spawned by the main agent. Omit this tool when you can continue useful work while agents run; completion notifications arrive automatically. Only the main agent can use this tool."
    }

    async fn call(&self, input: Self::Input, ctx: ToolExecutionContext) -> ToolResult {
        let task_ids = match normalize_wait_input(input) {
            Ok(task_ids) => task_ids,
            Err(error) => return error,
        };
        let Some(runtime) = ctx.runtime else {
            return ToolResult::error("wait_agents requires runtime context");
        };
        if runtime.agent_depth != 0 {
            return ToolResult::error("wait_agents is only available to the main agent");
        }
        let Some(supervisor) = &runtime.task_supervisor else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.wait_for_tasks(task_ids.as_deref()).await
    }
}
