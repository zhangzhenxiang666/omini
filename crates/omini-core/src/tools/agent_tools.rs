use crate::subagents::AgentTaskRequest;
use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentInput {
    pub name: String,
    pub prompt: String,
    pub title: String,
}

fn prepare_agent_input(input: AgentInput) -> Result<AgentTaskRequest, ToolResult> {
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

pub struct SpawnAgentTool;

#[async_trait]
impl Tool for SpawnAgentTool {
    type Input = AgentInput;
    type Prepared = AgentTaskRequest;

    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Start a named agent as a background task. Returns task_id and status immediately. When the task completes, you will receive an automatic notification in this conversation; do not wait or poll with Bash. You may call get_task with the task_id at any time to check its progress or read its terminal result. The child follows the owner thread's active profile. Only the main agent can use this tool."
    }

    async fn prepare(&self, input: AgentInput) -> Result<Self::Prepared, ToolResult> {
        prepare_agent_input(input)
    }

    async fn execute_prepared(
        &self,
        request: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let Some(runtime) = ctx.runtime.clone() else {
            return ToolResult::error("spawn_agent requires runtime context");
        };
        let Some(supervisor) = runtime.task_supervisor.clone() else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.spawn_background(request, ctx, runtime).await
    }
}

pub struct RunAgentTool;

#[async_trait]
impl Tool for RunAgentTool {
    type Input = AgentInput;
    type Prepared = AgentTaskRequest;

    fn name(&self) -> &str {
        "run_agent"
    }

    fn description(&self) -> &str {
        "Run a named child agent synchronously and return task_id, status, and its final result. The child follows the owner thread's active profile. Available only to agents below the maximum depth."
    }

    async fn prepare(&self, input: AgentInput) -> Result<Self::Prepared, ToolResult> {
        prepare_agent_input(input)
    }

    async fn execute_prepared(
        &self,
        request: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let Some(runtime) = ctx.runtime.clone() else {
            return ToolResult::error("run_agent requires runtime context");
        };
        let Some(supervisor) = runtime.task_supervisor.clone() else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.run_synchronous(request, ctx, runtime).await
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskIdInput {
    pub task_id: String,
}

fn prepare_task_id(input: TaskIdInput) -> Result<String, ToolResult> {
    let task_id = input.task_id.trim();
    if task_id.is_empty() {
        return Err(ToolResult::error("task_id must not be empty"));
    }
    Ok(task_id.to_string())
}

pub struct GetTaskTool;

#[async_trait]
impl Tool for GetTaskTool {
    type Input = TaskIdInput;
    type Prepared = String;

    fn name(&self) -> &str {
        "get_task"
    }

    fn description(&self) -> &str {
        "Read an agent task's task_id, status, and optional terminal output/error/warnings. Only the main agent can use this tool."
    }

    async fn prepare(&self, input: TaskIdInput) -> Result<Self::Prepared, ToolResult> {
        prepare_task_id(input)
    }

    async fn execute_prepared(&self, task_id: String, ctx: ToolExecutionContext) -> ToolResult {
        let Some(runtime) = ctx.runtime else {
            return ToolResult::error("get_task requires runtime context");
        };
        if runtime.agent_depth != 0 {
            return ToolResult::error("get_task is only available to the main agent");
        }
        let Some(supervisor) = &runtime.task_supervisor else {
            return ToolResult::error("agent task supervisor is not available");
        };
        supervisor.get_task(&task_id)
    }
}

pub struct CancelTaskTool;

#[async_trait]
impl Tool for CancelTaskTool {
    type Input = TaskIdInput;
    type Prepared = String;

    fn name(&self) -> &str {
        "cancel_task"
    }

    fn description(&self) -> &str {
        "Idempotently cancel one agent task and all of its descendants without affecting sibling tasks. Only the main agent can use this tool."
    }

    async fn prepare(&self, input: TaskIdInput) -> Result<Self::Prepared, ToolResult> {
        prepare_task_id(input)
    }

    async fn execute_prepared(&self, task_id: String, ctx: ToolExecutionContext) -> ToolResult {
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

#[cfg(test)]
mod tests {
    use crate::tools::Tool;

    use super::*;

    #[test]
    fn agent_schemas_require_name_prompt_and_title() {
        for schema in [SpawnAgentTool.input_schema(), RunAgentTool.input_schema()] {
            let required = schema["required"].as_array().expect("required fields");
            for field in ["name", "prompt", "title"] {
                assert!(required.iter().any(|value| value.as_str() == Some(field)));
            }
        }
    }

    #[test]
    fn spawn_agent_description_explains_completion_notification() {
        let description = SpawnAgentTool.description();

        assert!(description.contains("automatic notification"));
        assert!(description.contains("do not wait or poll with Bash"));
        assert!(description.contains("get_task"));
    }
}
