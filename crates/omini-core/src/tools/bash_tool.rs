use super::{Tool, ToolExecutionContext, ToolPolicy, ToolResult};
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
pub struct BashPermissionPolicy;

#[async_trait]
impl ToolPolicy<BashTool> for BashPermissionPolicy {
    fn normalize_raw_input(&self, raw_input: &mut serde_json::Value, cwd: &std::path::Path) {
        super::normalize_path_field(raw_input, "workdir", cwd, true);
    }

    async fn preflight(
        &self,
        input: &BashInput,
        _ctx: &ToolExecutionContext,
    ) -> Result<Option<PermissionPreview>, ToolResult> {
        Ok(Some(PermissionPreview::Bash(BashPermissionPreview {
            command: input.command.clone(),
            description: input.description.clone(),
            workdir: input.workdir.clone(),
            timeout: input.timeout.unwrap_or(120_000).min(600_000),
        })))
    }
}

#[async_trait]
impl Tool for BashTool {
    type Input = BashInput;

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

    async fn call(&self, input: BashInput, _ctx: ToolExecutionContext) -> ToolResult {
        let timeout = input.timeout.unwrap_or(120_000).min(600_000);
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&input.command).envs(std::env::vars());
        cmd.kill_on_drop(true);

        // 设置工作目录
        if let Some(ref workdir) = input.workdir {
            cmd.current_dir(workdir);
        }

        // 超时控制（默认 120s，最大 600s）
        let timeout_dur = Duration::from_millis(timeout);

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

        // stdout/stderr 各按字节上限截断：命令输出（如 cat 大文件）可能巨量，
        // 未截断会拖垮持久化（sidecar 写盘）与事件广播链路
        let stdout = truncate_command_output(&output.stdout);
        let stderr = truncate_command_output(&output.stderr);

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

/// 命令单个输出流的字节上限。
const OUTPUT_BYTE_LIMIT: usize = 256 * 1024;

/// 按字节上限截断命令输出（UTF-8 安全边界），并附截断提示。
fn truncate_command_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).into_owned();
    if text.len() <= OUTPUT_BYTE_LIMIT {
        return text;
    }
    let mut end = OUTPUT_BYTE_LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n(... output truncated: kept first {end} of {} bytes ...)",
        &text[..end],
        text.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_command_output_caps_oversized_stream() {
        let oversized = vec![b'b'; OUTPUT_BYTE_LIMIT * 4];
        let truncated = truncate_command_output(&oversized);

        assert!(truncated.len() < OUTPUT_BYTE_LIMIT + 200);
        assert!(truncated.starts_with('b'));
        assert!(truncated.contains("output truncated"));
    }

    #[test]
    fn truncate_command_output_keeps_small_stream_unchanged() {
        assert_eq!(truncate_command_output(b"ok"), "ok");
        assert_eq!(truncate_command_output(&[]), "");
    }
}
