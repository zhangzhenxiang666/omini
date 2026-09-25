use crate::subagents::AgentTaskRequest;
use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnAgentInput {
    /// Name of an available agent from the agent list in the system instructions.
    pub name: String,
    /// Self-contained task brief with the goal, relevant context, expected output, and constraints.
    pub prompt: String,
    /// Short user-facing label for this task, written in the user's language.
    pub title: String,
}

pub struct SpawnAgentTool;

fn prepare_input(input: SpawnAgentInput) -> Result<AgentTaskRequest, ToolResult> {
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
impl Tool for SpawnAgentTool {
    type Input = SpawnAgentInput;

    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Start a named agent as a background task. Returns task_id and status immediately. Completion is reported automatically; usually continue your work without polling or reading task status. If this turn needs to wait for results, use wait_agents. The child follows the owner thread's active profile. Only the main agent can use this tool."
    }

    async fn call(&self, input: Self::Input, ctx: ToolExecutionContext) -> ToolResult {
        let request = match prepare_input(input) {
            Ok(request) => request,
            Err(result) => return result,
        };
        let Some(runtime) = ctx.runtime.clone() else {
            return ToolResult::error("spawn_agent requires runtime context");
        };
        let Some(supervisor) = runtime.task_supervisor.clone() else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.spawn_background(request, ctx, runtime).await
    }
}
