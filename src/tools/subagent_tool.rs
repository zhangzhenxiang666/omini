use super::{Tool, ToolExecutionContext, ToolResult};
use crate::subagents::SubagentRunRequest;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SubagentInput {
    /// Subagent name to run.
    pub name: String,
    /// Task prompt for the subagent. The parent agent must describe the concrete task.
    pub prompt: String,
    /// Optional short title shown in the UI for this subagent task.
    pub title: Option<String>,
}

pub struct SubagentTool;

#[async_trait]
impl Tool for SubagentTool {
    type Input = SubagentInput;
    type Prepared = SubagentRunRequest;

    fn name(&self) -> &str {
        "subagent"
    }

    fn description(&self) -> &str {
        concat!(
            "Run an isolated subagent for a focused task and wait for its final result.\n",
            "\n",
            "Input fields:\n",
            "  name    The subagent name to run.\n",
            "  prompt  The concrete task for that subagent. This field is required.\n",
            "  title   Optional short UI title for this subagent task.\n",
            "\n",
            "The subagent has its own session, context, system instructions, ",
            "and tool allowlist. Its intermediate messages are hidden from the main context. ",
            "You may call this tool multiple times in one assistant turn to run subagents in parallel.\n",
            "\n",
            "Built-in subagents: default, explorer, worker. Custom subagents are loaded from ",
            ".omini/agents/*.md in the current workspace."
        )
    }

    async fn prepare(&self, input: SubagentInput) -> Result<Self::Prepared, ToolResult> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(ToolResult::error("name must not be empty"));
        }
        if input.prompt.trim().is_empty() {
            return Err(ToolResult::error("prompt must not be empty"));
        }
        let title = input.title.and_then(|title| {
            let title = title.trim();
            if title.is_empty() {
                None
            } else {
                Some(title.chars().take(80).collect())
            }
        });
        Ok(SubagentRunRequest {
            name: name.to_string(),
            prompt: input.prompt,
            title,
        })
    }

    async fn execute_prepared(
        &self,
        request: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let Some(runtime) = ctx.runtime.clone() else {
            return ToolResult::error("subagent requires runtime context");
        };
        if runtime.session_type == "subagent" {
            return ToolResult::error("subagent tool is not available inside subagents");
        }
        let Some(runner) = runtime.subagent_runner.clone() else {
            return ToolResult::error("subagent runner is not available");
        };

        runner.run_subagent(request, ctx, runtime).await
    }
}
