use super::{Tool, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashInput {
    /// The shell command to execute
    pub command: String,
    /// Clear, concise description of what this command does (e.g. "List files in current directory").
    #[serde(default)]
    pub description: Option<String>,
    /// Optional timeout in milliseconds (default: 120000, max: 600000).
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Optional working directory. Use this instead of `cd` in the command.
    #[serde(default)]
    pub workdir: Option<String>,
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    type Input = BashInput;

    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command and returns its output. The working directory persists between commands, but shell state does not."
    }

    async fn execute(&self, input: BashInput) -> ToolResult {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&input.command).envs(std::env::vars());

        // 设置工作目录
        if let Some(ref workdir) = input.workdir {
            cmd.current_dir(workdir);
        }

        // 超时控制（默认 120s，最大 600s）
        let timeout_dur = Duration::from_millis(input.timeout.unwrap_or(120_000).min(600_000));

        let output = match tokio::time::timeout(timeout_dur, cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return ToolResult::error(format!("Failed to spawn shell: {e}")),
            Err(_) => {
                return ToolResult::error(format!(
                    "Command timed out after {}ms",
                    timeout_dur.as_millis()
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let result = if output.status.success() {
            if stdout.is_empty() {
                "(no output)".to_string()
            } else {
                stdout
            }
        } else {
            let exit_code = output.status.code().map_or("?".into(), |c| c.to_string());
            format!("Exit code: {exit_code}\n{stdout}\n{stderr}")
        };

        ToolResult::ok(result.trim())
    }
}
