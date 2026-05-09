use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Tool, ToolResult};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashInput {
    /// Shell command to execute
    pub command: String,
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    type Input = BashInput;

    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command"
    }

    async fn execute(&self, input: BashInput) -> ToolResult {
        let output = match std::process::Command::new("sh")
            .arg("-c")
            .arg(&input.command)
            .output()
        {
            Ok(o) => o,
            Err(e) => return ToolResult::error(format!("Failed to spawn shell: {e}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let result = if output.status.success() {
            stdout
        } else {
            let exit_code = output.status.code().map_or("?".into(), |c| c.to_string());
            format!("Exit code: {exit_code}\n{stdout}\n{stderr}")
        };

        ToolResult::ok(result.trim())
    }
}
