use crate::subagents::AgentTaskRequest;
use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunAgentInput {
    /// Name of an available agent from the agent list in the system instructions.
    pub name: String,
    /// Self-contained task brief with the goal, relevant context, expected output, and constraints.
    pub prompt: String,
    /// Short user-facing label for this task, written in the user's language.
    pub title: String,
}

pub struct RunAgentTool;

fn prepare_input(input: RunAgentInput) -> Result<AgentTaskRequest, ToolResult> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ToolResult::error("name must not be empty"));
    }
    if input.prompt.trim().is_empty() {
        return Err(ToolResult::error("prompt must not be empty"));
    }
    let title = input.title.trim();
    if title.is_empty() {
        return Err(ToolResult::error("title must not be empty"));
    }
    Ok(AgentTaskRequest {
        name: name.to_string(),
        prompt: input.prompt,
        title: title.chars().take(80).collect(),
    })
}

#[async_trait]
impl Tool for RunAgentTool {
    type Input = RunAgentInput;

    fn name(&self) -> &str {
        "run_agent"
    }

    fn description(&self) -> &str {
        "Run a named child agent synchronously and return task_id, status, and its final result. The child follows the owner thread's active profile. Available only to agents below the maximum depth."
    }

    async fn call(&self, input: Self::Input, ctx: ToolExecutionContext) -> ToolResult {
        let request = match prepare_input(input) {
            Ok(request) => request,
            Err(result) => return result,
        };
        let Some(runtime) = ctx.runtime.clone() else {
            return ToolResult::error("run_agent requires runtime context");
        };
        let Some(supervisor) = runtime.task_supervisor.clone() else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.run_synchronous(request, ctx, runtime).await
    }
}
