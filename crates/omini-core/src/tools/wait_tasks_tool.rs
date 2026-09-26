use crate::tools::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitTasksInput {
    /// Task IDs to wait for. Waits for all listed tasks; omit this field to wait for every
    /// currently running background task owned by the main thread.
    #[serde(default)]
    pub task_ids: Option<Vec<String>>,
}

pub struct WaitTasksTool;

pub(super) fn normalize_wait_input(
    input: WaitTasksInput,
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
impl Tool for WaitTasksTool {
    type Input = WaitTasksInput;

    fn name(&self) -> &str {
        "wait_tasks"
    }

    fn description(&self) -> &str {
        "Wait for all listed background tasks to finish and return their terminal results. If task_ids is omitted, wait for all currently running tasks owned by the main thread. Omit this tool when you can continue useful work while tasks run; completion notifications arrive automatically. Only the main agent can use this tool."
    }

    async fn call(&self, input: Self::Input, ctx: ToolExecutionContext) -> ToolResult {
        let task_ids = match normalize_wait_input(input) {
            Ok(task_ids) => task_ids,
            Err(error) => return error,
        };
        let Some(runtime) = ctx.runtime else {
            return ToolResult::error("wait_tasks requires runtime context");
        };
        if runtime.agent_depth != 0 {
            return ToolResult::error("wait_tasks is only available to the main agent");
        }
        let Some(manager) = &runtime.task_manager else {
            return ToolResult::error("task manager is not available");
        };
        let supervisor = runtime.task_supervisor.as_ref();
        let task_ids = task_ids.unwrap_or_else(|| manager.active_ids(&runtime.owner_thread_id));
        match manager.wait_for(&runtime.owner_thread_id, &task_ids).await {
            Ok(tasks) => {
                let subagent_ids = tasks
                    .iter()
                    .filter(|task| task.kind == omini_domain::task::TaskKind::SubAgent)
                    .map(|task| task.task_id.clone())
                    .collect::<Vec<_>>();
                if subagent_ids.is_empty() {
                    return ToolResult::ok(
                        serde_json::to_string(&tasks).unwrap_or_else(|_| "[]".to_string()),
                    );
                }
                let Some(supervisor) = supervisor else {
                    return ToolResult::error("agent task supervisor is not available");
                };
                let subagent_results = supervisor.wait_for_tasks(Some(&subagent_ids)).await;
                if subagent_ids.len() == tasks.len() {
                    return subagent_results;
                }
                if subagent_results.is_error {
                    return subagent_results;
                }
                let subagent_results =
                    serde_json::from_str::<serde_json::Value>(&subagent_results.output)
                        .unwrap_or(serde_json::Value::Null);
                ToolResult::ok(
                    serde_json::json!({
                        "tasks": tasks,
                        "subagent_results": subagent_results,
                    })
                    .to_string(),
                )
            }
            Err(error) => ToolResult::error(error),
        }
    }
}
