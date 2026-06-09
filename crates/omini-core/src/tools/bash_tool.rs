use super::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use omini_domain::events::{BashPermissionPreview, PermissionPreview};
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

#[derive(Debug)]
pub struct PreparedBash {
    pub command: String,
    pub description: Option<String>,
    pub timeout: u64,
    pub workdir: Option<String>,
}

#[async_trait]
impl Tool for BashTool {
    type Input = BashInput;
    type Prepared = PreparedBash;

    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        concat!(
            "Execute a shell command and return its output.\n",
            "The working directory persists between commands, but shell state does not.\n",
            "\n",
            "Best for: running build tools (cargo, make), git operations, package managers,\n",
            "test commands, dev servers, and external CLIs that are not covered by a dedicated tool.\n",
            "\n",
            "Not for local project search. Do not use this tool to run `rg`, `grep`, `find`,\n",
            "or `ls` when the goal is finding files or matching code. Use `search` instead.\n",
            "\n",
            "For file operations, prefer these dedicated tools instead of using shell commands:\n",
            "  search       Search file contents or file paths using ripgrep\n",
            "  read         Read file contents (with line numbers and offset/limit support)\n",
            "  edit         Edit an existing text file by exact string replacement\n",
            "  write        Create a new text file or fully overwrite an existing text file\n",
            "\n",
            "Avoid using grep/find/rg for normal project search — use the `search` tool instead.\n",
            "Avoid using cat/head/tail/sed/awk for file reads — use the `read` tool instead.\n",
            "Avoid using sed/echo/redirect for file edits — use the `edit` or `write` tool instead."
        )
    }

    async fn prepare(&self, input: BashInput) -> Result<Self::Prepared, ToolResult> {
        Ok(PreparedBash {
            command: input.command,
            description: input.description,
            timeout: input.timeout.unwrap_or(120_000).min(600_000),
            workdir: input.workdir,
        })
    }

    fn permission_preview(&self, prepared: &Self::Prepared) -> Option<PermissionPreview> {
        Some(PermissionPreview::Bash(BashPermissionPreview {
            command: prepared.command.clone(),
            description: prepared.description.clone(),
            workdir: prepared.workdir.clone(),
            timeout: prepared.timeout,
        }))
    }

    async fn execute_prepared(
        &self,
        input: Self::Prepared,
        _ctx: ToolExecutionContext,
    ) -> ToolResult {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&input.command).envs(std::env::vars());
        cmd.kill_on_drop(true);

        // 设置工作目录
        if let Some(ref workdir) = input.workdir {
            cmd.current_dir(workdir);
        }

        // 超时控制（默认 120s，最大 600s）
        let timeout_dur = Duration::from_millis(input.timeout);

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
