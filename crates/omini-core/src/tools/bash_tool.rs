use super::{Tool, ToolExecutionContext, ToolPolicy, ToolResult};
use crate::tasks::{BackgroundTaskReservation, TaskCancellation, TaskManager};
use async_trait::async_trait;
use chrono::Utc;
use omini_domain::events::{BashPermissionPreview, PermissionPreview};
use omini_domain::task::{TaskInfo, TaskKind, TaskOutputDelta, TaskOutputStream, TaskStatus};
use schemars::JsonSchema;
use serde::Deserialize;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

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
            "Execute a shell command and return its output. Main-agent commands that run longer than 30 seconds are automatically continued as background tasks when capacity is available. Background task management is only available to the main agent.\n",
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
            "  write        Create a new text file or fully overwrite an existing file\n",
            "\n",
            "Avoid using grep/find/rg for normal project search — use the `search` tool instead.\n",
            "Avoid using cat/head/tail/sed/awk for file reads — use the `read` tool instead.\n",
            "Avoid using sed/echo/redirect for file edits — use the `edit` or `write` tool instead."
        )
    }

    async fn call(&self, input: BashInput, ctx: ToolExecutionContext) -> ToolResult {
        let timeout = Duration::from_millis(input.timeout.unwrap_or(120_000).min(600_000));
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&input.command)
            .envs(std::env::vars())
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(workdir) = &input.workdir {
            command.current_dir(workdir);
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return ToolResult::error(format!("Failed to spawn shell: {error}")),
        };

        let task_cancelled = Arc::new(AtomicBool::new(false));
        let task_cancel_notify = Arc::new(Notify::new());
        let (mut output_rx, process) = start_process(
            child,
            timeout,
            Arc::clone(&ctx.cancelled),
            Arc::clone(&ctx.cancel_notify),
            Arc::clone(&task_cancelled),
            Arc::clone(&task_cancel_notify),
        );
        let mut process = Some(process);
        let manager = ctx
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.task_manager.clone());
        let can_background = ctx
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.agent_depth == 0)
            && manager.is_some();
        let task_id = ctx.tool_use_id.clone();
        let started_at = Instant::now();
        let started_at_utc = Utc::now();
        let mut deadline = Box::pin(tokio::time::sleep(Duration::from_secs(30)));
        let mut background_attempted = false;
        let mut output = OutputTail::default();

        loop {
            tokio::select! {
                biased;
                chunk = output_rx.recv() => match chunk {
                    Some(chunk) => {
                        output.push(chunk.stream, &chunk.delta);
                        if let Some(manager) = &manager {
                            manager.output(TaskOutputDelta {
                                task_id: task_id.clone(),
                                tool_use_id: task_id.clone(),
                                stream: chunk.stream,
                                delta: chunk.delta,
                            }).await;
                        }
                    }
                    None => {
                        let result = process.take().expect("process handle is present").await;
                        return foreground_result(result, output, timeout);
                    }
                },
                result = async { process.as_mut().expect("process handle is present").await } => {
                    return foreground_result(result, output, timeout);
                }
                _ = &mut deadline, if can_background && !background_attempted => {
                    background_attempted = true;
                    let Some(manager) = manager.clone() else {
                        continue;
                    };
                    let reservation = match manager.reserve_background() {
                        Ok(reservation) => reservation,
                        Err(_) => continue,
                    };
                    let task = TaskInfo {
                        task_id: task_id.clone(),
                        owner_thread_id: ctx.runtime.as_ref().expect("checked runtime").owner_thread_id.clone(),
                        kind: TaskKind::Bash,
                        title: task_title(&input),
                        status: TaskStatus::Running,
                        created_at: started_at_utc,
                        updated_at: Utc::now(),
                        completed_at: None,
                        result_summary: None,
                    };
                    if manager
                        .register(
                            task.clone(),
                            Some(TaskCancellation::new(
                                Arc::clone(&task_cancelled),
                                Arc::clone(&task_cancel_notify),
                            )),
                        )
                        .await
                        .is_err()
                    {
                        drop(reservation);
                        continue;
                    }
                    tokio::spawn(continue_background(
                        manager,
                        task,
                        output_rx,
                        process.take().expect("process handle is present"),
                        output,
                        timeout,
                        reservation,
                    ));
                    return ToolResult::ok(
                        serde_json::json!({
                            "task_id": task_id,
                            "status": "running",
                            "background": true,
                            "elapsed_ms": started_at.elapsed().as_millis(),
                        })
                        .to_string(),
                    );
                }
            }
        }
    }
}

struct OutputChunk {
    stream: TaskOutputStream,
    delta: String,
}

struct ProcessResult {
    status: Option<ExitStatus>,
    timed_out: bool,
    cancelled: bool,
    error: Option<String>,
}

fn start_process(
    mut child: Child,
    timeout: Duration,
    run_cancelled: Arc<AtomicBool>,
    run_cancel_notify: Arc<Notify>,
    task_cancelled: Arc<AtomicBool>,
    task_cancel_notify: Arc<Notify>,
) -> (mpsc::Receiver<OutputChunk>, JoinHandle<ProcessResult>) {
    let (output_tx, output_rx) = mpsc::channel(64);
    let (stdout_tx, stderr_tx) = (output_tx.clone(), output_tx.clone());
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(tokio::spawn(read_output(
            stdout,
            TaskOutputStream::Stdout,
            stdout_tx,
        )));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(tokio::spawn(read_output(
            stderr,
            TaskOutputStream::Stderr,
            stderr_tx,
        )));
    }
    drop(output_tx);

    let process = tokio::spawn(async move {
        let timeout_sleep = tokio::time::sleep(timeout);
        tokio::pin!(timeout_sleep);
        let (status, timed_out, cancelled, error) = tokio::select! {
            result = child.wait() => match result {
                Ok(status) => (Some(status), false, false, None),
                Err(error) => (None, false, false, Some(error.to_string())),
            },
            _ = wait_for_cancel(&run_cancelled, &run_cancel_notify), if !task_cancelled.load(Ordering::Relaxed) => {
                let _ = child.kill().await;
                let result = child.wait().await;
                match result {
                    Ok(status) => (Some(status), false, true, None),
                    Err(error) => (None, false, true, Some(error.to_string())),
                }
            }
            _ = wait_for_cancel(&task_cancelled, &task_cancel_notify) => {
                let _ = child.kill().await;
                let result = child.wait().await;
                match result {
                    Ok(status) => (Some(status), false, true, None),
                    Err(error) => (None, false, true, Some(error.to_string())),
                }
            }
            _ = &mut timeout_sleep => {
                let _ = child.kill().await;
                let result = child.wait().await;
                match result {
                    Ok(status) => (Some(status), true, false, None),
                    Err(error) => (None, true, false, Some(error.to_string())),
                }
            }
        };
        for reader in readers {
            if let Err(join_error) = reader.await
                && error.is_none()
            {
                tracing::debug!(error = %join_error, "bash output reader stopped");
            }
        }
        ProcessResult {
            status,
            timed_out,
            cancelled,
            error,
        }
    });
    (output_rx, process)
}

async fn wait_for_cancel(cancelled: &AtomicBool, notify: &Notify) {
    loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        notified.await;
    }
}

async fn read_output<R>(
    mut reader: R,
    stream: TaskOutputStream,
    output_tx: mpsc::Sender<OutputChunk>,
) where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 4096];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => {
                if let Some(delta) = decode_utf8(&mut pending, &[], true)
                    && !delta.is_empty()
                {
                    let _ = output_tx.send(OutputChunk { stream, delta }).await;
                }
                return;
            }
            Ok(read) => {
                if let Some(delta) = decode_utf8(&mut pending, &buffer[..read], false)
                    && !delta.is_empty()
                    && output_tx.send(OutputChunk { stream, delta }).await.is_err()
                {
                    return;
                }
            }
            Err(error) => {
                tracing::debug!(%error, ?stream, "failed to read bash output");
                return;
            }
        }
    }
}

fn decode_utf8(pending: &mut Vec<u8>, incoming: &[u8], eof: bool) -> Option<String> {
    pending.extend_from_slice(incoming);
    let mut decoded = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                decoded.push_str(valid);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid_end = error.valid_up_to();
                if valid_end > 0 {
                    decoded.push_str(std::str::from_utf8(&pending[..valid_end]).ok()?);
                    pending.drain(..valid_end);
                    continue;
                }
                if let Some(invalid_len) = error.error_len() {
                    decoded.push_str(&String::from_utf8_lossy(&pending[..invalid_len]));
                    pending.drain(..invalid_len);
                    continue;
                }
                if eof {
                    decoded.push_str(&String::from_utf8_lossy(pending));
                    pending.clear();
                }
                break;
            }
        }
    }
    Some(decoded)
}

async fn continue_background(
    manager: Arc<TaskManager>,
    task: TaskInfo,
    mut output_rx: mpsc::Receiver<OutputChunk>,
    process: JoinHandle<ProcessResult>,
    mut output: OutputTail,
    timeout: Duration,
    _reservation: BackgroundTaskReservation,
) {
    while let Some(chunk) = output_rx.recv().await {
        output.push(chunk.stream, &chunk.delta);
        manager
            .output(TaskOutputDelta {
                task_id: task.task_id.clone(),
                tool_use_id: task.task_id.clone(),
                stream: chunk.stream,
                delta: chunk.delta,
            })
            .await;
    }
    let process = match process.await {
        Ok(process) => process,
        Err(error) => ProcessResult {
            status: None,
            timed_out: false,
            cancelled: false,
            error: Some(format!("Bash task failed: {error}")),
        },
    };
    let status = if process.cancelled {
        TaskStatus::Cancelled
    } else if process.timed_out || process.error.is_some() {
        TaskStatus::Failed
    } else if process.status.is_some_and(|status| status.success()) {
        TaskStatus::Completed
    } else {
        TaskStatus::Failed
    };
    let summary = format_process_result(&process, &output, Some(timeout));
    if let Err(error) = manager
        .complete(&task.task_id, status, "Bash".to_string(), summary)
        .await
    {
        tracing::warn!(task_id = %task.task_id, %error, "failed to finish background Bash task");
    }
}

fn foreground_result(
    result: Result<ProcessResult, tokio::task::JoinError>,
    output: OutputTail,
    timeout: Duration,
) -> ToolResult {
    let result = match result {
        Ok(result) => result,
        Err(error) => return ToolResult::error(format!("Bash command failed: {error}")),
    };
    if result.cancelled {
        return ToolResult::error("Command cancelled");
    }
    if result.timed_out {
        return ToolResult::error(format!("Command timed out after {}ms", timeout.as_millis()));
    }
    if let Some(error) = result.error {
        return ToolResult::error(format!("Failed to wait for shell: {error}"));
    }
    let stdout = output.stdout;
    let stderr = output.stderr;
    let success = result.status.is_some_and(|status| status.success());
    let text = if success {
        if stdout.is_empty() {
            "(no output)".to_string()
        } else {
            stdout
        }
    } else {
        let exit_code = result
            .status
            .and_then(|status| status.code())
            .map_or_else(|| "?".to_string(), |code| code.to_string());
        format!("Exit code: {exit_code}\n{stdout}\n{stderr}")
    };
    ToolResult::ok(text.trim())
}

fn format_process_result(
    result: &ProcessResult,
    output: &OutputTail,
    timeout: Option<Duration>,
) -> String {
    if result.cancelled {
        return format!("Command cancelled\n{}", output.combined());
    }
    if result.timed_out {
        let timeout = timeout.map_or(600_000, |timeout| timeout.as_millis());
        return format!("Command timed out after {timeout}ms\n{}", output.combined());
    }
    if let Some(error) = &result.error {
        return format!("{error}\n{}", output.combined());
    }
    let exit_code = result
        .status
        .and_then(|status| status.code())
        .map_or_else(|| "?".to_string(), |code| code.to_string());
    format!("Exit code: {exit_code}\n{}", output.combined())
}

fn task_title(input: &BashInput) -> String {
    input
        .description
        .as_deref()
        .filter(|description| !description.trim().is_empty())
        .unwrap_or(&input.command)
        .chars()
        .take(120)
        .collect()
}

#[derive(Default)]
struct OutputTail {
    stdout: String,
    stderr: String,
}

impl OutputTail {
    fn push(&mut self, stream: TaskOutputStream, delta: &str) {
        let output = match stream {
            TaskOutputStream::Stdout => &mut self.stdout,
            TaskOutputStream::Stderr => &mut self.stderr,
        };
        output.push_str(delta);
        if output.len() > OUTPUT_BYTE_LIMIT {
            let mut start = output.len() - OUTPUT_BYTE_LIMIT;
            while !output.is_char_boundary(start) {
                start += 1;
            }
            output.drain(..start);
        }
    }

    fn combined(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout, self.stderr),
        }
    }
}

/// 每个输出流在最终结果和内存快照中保留的最大字节数。
const OUTPUT_BYTE_LIMIT: usize = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_tail_keeps_recent_bytes_without_splitting_utf8() {
        let mut output = OutputTail::default();
        output.push(TaskOutputStream::Stdout, &"a".repeat(OUTPUT_BYTE_LIMIT));
        output.push(TaskOutputStream::Stdout, "中");

        assert!(output.stdout.len() <= OUTPUT_BYTE_LIMIT);
        assert!(output.stdout.ends_with('中'));
    }

    #[test]
    fn utf8_decoder_preserves_characters_split_across_reads() {
        let bytes = "中".as_bytes();
        let mut pending = Vec::new();

        assert_eq!(
            decode_utf8(&mut pending, &bytes[..1], false),
            Some(String::new())
        );
        assert_eq!(
            decode_utf8(&mut pending, &bytes[1..], false),
            Some("中".to_string())
        );
    }

    #[test]
    fn utf8_decoder_replaces_invalid_bytes() {
        let mut pending = Vec::new();
        assert_eq!(
            decode_utf8(&mut pending, &[0xff], true),
            Some("�".to_string())
        );
    }
}
