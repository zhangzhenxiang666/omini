use super::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendMessageInput {
    /// Recipient task ID, or `parent` when called by a first-level child agent.
    pub target: String,
    /// Message content to inject at the recipient's next safe boundary.
    pub message: String,
}

pub struct SendMessageTool;

#[async_trait]
impl Tool for SendMessageTool {
    type Input = SendMessageInput;

    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to a direct child agent by task ID, or to the parent when called by a first-level child. The recipient receives it at the next safe boundary."
    }

    async fn call(&self, input: Self::Input, ctx: ToolExecutionContext) -> ToolResult {
        let target = input.target.trim();
        let text = input.message.trim();
        if target.is_empty() || text.is_empty() {
            return ToolResult::error("target and message must not be empty");
        }
        let Some(runtime) = ctx.runtime else {
            return ToolResult::error("send_message requires runtime context");
        };
        let Some(supervisor) = &runtime.task_supervisor else {
            return ToolResult::error("agent task supervisor is not available");
        };
        let message = omini_model::message::Message::from_user_text(text.to_string());
        match supervisor.send_message(
            runtime.task_id.as_deref(),
            runtime.agent_depth,
            target,
            message,
        ) {
            Ok(()) => ToolResult::ok("Message queued for the recipient's next safe boundary."),
            Err(error) => ToolResult::error(error),
        }
    }
}
