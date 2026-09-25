use crate::subagents::AgentTaskRequest;
use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentInput {
    /// Name of an available agent from the agent list in the system instructions.
    pub name: String,
    /// Self-contained task brief with the goal, relevant context, expected output, and constraints.
    pub prompt: String,
    /// Short user-facing label for this task, written in the user's language.
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
        "Start a named agent as a background task. Returns task_id and status immediately. Completion is reported automatically; usually continue your work without polling or reading task status. If this turn needs to wait for results, use wait_agents. The child follows the owner thread's active profile. Only the main agent can use this tool."
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
    /// Task ID returned by `spawn_agent`.
    pub task_id: String,
}

fn prepare_task_id(input: TaskIdInput) -> Result<String, ToolResult> {
    let task_id = input.task_id.trim();
    if task_id.is_empty() {
        return Err(ToolResult::error("task_id must not be empty"));
    }
    Ok(task_id.to_string())
}

pub struct ReadTaskTool;

#[async_trait]
impl Tool for ReadTaskTool {
    type Input = TaskIdInput;
    type Prepared = String;

    fn name(&self) -> &str {
        "read_task"
    }

    fn description(&self) -> &str {
        "Read an agent task's current status and optional terminal output/error/warnings. Usually rely on automatic completion notifications; use this only when you need to inspect a task without waiting. Only the main agent can use this tool."
    }

    async fn prepare(&self, input: TaskIdInput) -> Result<Self::Prepared, ToolResult> {
        prepare_task_id(input)
    }

    async fn execute_prepared(&self, task_id: String, ctx: ToolExecutionContext) -> ToolResult {
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitAgentsInput {
    /// Task IDs to wait for. Waits for all listed tasks; omit this field to wait for every
    /// currently running background task spawned by the main agent.
    #[serde(default)]
    pub task_ids: Option<Vec<String>>,
}

pub struct WaitAgentsTool;

#[async_trait]
impl Tool for WaitAgentsTool {
    type Input = WaitAgentsInput;
    type Prepared = Option<Vec<String>>;

    fn name(&self) -> &str {
        "wait_agents"
    }

    fn description(&self) -> &str {
        "Wait for all listed agent tasks to finish and return their terminal results. If task_ids is omitted, wait for all currently running tasks spawned by the main agent. Omit this tool when you can continue useful work while agents run; completion notifications arrive automatically. Only the main agent can use this tool."
    }

    async fn prepare(&self, input: WaitAgentsInput) -> Result<Self::Prepared, ToolResult> {
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

    async fn execute_prepared(
        &self,
        task_ids: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
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
