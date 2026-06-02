use super::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TodoWriteInput {
    /// Current execution todo list. Send the full current list each time it changes.
    pub todos: Vec<TodoItemInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TodoItemInput {
    /// Brief description of the task.
    pub content: String,
    /// Current status for this task.
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    type Input = TodoWriteInput;
    type Prepared = TodoWriteInput;

    fn name(&self) -> &str {
        "todo_write"
    }

    fn description(&self) -> &str {
        concat!(
            "Track execution progress for approved implementation work.\n",
            "\n",
            "Only use this in main profile after the user asks for implementation or approves ",
            "a plan. Send the full current todo list whenever statuses change. This is for ",
            "execution progress, not plan-profile planning discussion.\n",
            "\n",
            "Statuses: pending, in_progress, completed, cancelled."
        )
    }

    async fn prepare(&self, input: TodoWriteInput) -> Result<Self::Prepared, ToolResult> {
        if input.todos.is_empty() {
            return Err(ToolResult::error("todos must contain at least one item"));
        }
        for todo in &input.todos {
            if todo.content.trim().is_empty() {
                return Err(ToolResult::error("todo content must not be empty"));
            }
        }
        Ok(input)
    }

    async fn execute_prepared(
        &self,
        input: Self::Prepared,
        _ctx: ToolExecutionContext,
    ) -> ToolResult {
        ToolResult::ok(
            serde_json::to_string_pretty(&input)
                .unwrap_or_else(|_| format!("Updated {} todo item(s)", input.todos.len())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_empty_todos() {
        let err = TodoWriteTool
            .prepare(TodoWriteInput { todos: Vec::new() })
            .await
            .unwrap_err();

        assert!(err.output.contains("at least one"));
    }

    #[tokio::test]
    async fn rejects_empty_content() {
        let err = TodoWriteTool
            .prepare(TodoWriteInput {
                todos: vec![TodoItemInput {
                    content: " ".to_string(),
                    status: TodoStatus::Pending,
                }],
            })
            .await
            .unwrap_err();

        assert!(err.output.contains("content"));
    }
}
