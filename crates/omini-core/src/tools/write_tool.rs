use super::{Tool, ToolExecutionContext, ToolResult, tool_metadata};
use crate::util::file_lock::FileLockService;
use async_trait::async_trait;
use omini_domain::events::{EditPermissionPreview, PermissionPreview};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use similar::TextDiff;
use std::path::Path;
use tokio::fs;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WriteInput {
    /// The absolute path to the file to write.
    pub file_path: String,
    /// Complete UTF-8 text content to write to the file.
    pub content: String,
}

pub struct WriteTool;

#[derive(Debug)]
pub struct PreparedWrite {
    input: WriteInput,
    preview: EditPermissionPreview,
    existed: bool,
}

#[async_trait]
impl Tool for WriteTool {
    type Input = WriteInput;
    type Prepared = PreparedWrite;

    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        concat!(
            "Write a complete UTF-8 text file to the local filesystem.\n",
            "\n",
            "Use this to create a new file or fully overwrite an existing file.\n",
            "\n",
            "Input:\n",
            "  file_path  Absolute path to the file.\n",
            "  content    Complete file content to write.\n",
            "\n",
            "Rules:\n",
            "  - file_path must be absolute; relative paths are rejected.\n",
            "  - Missing parent directories are created automatically after permission approval.\n",
            "  - The target path must not be a directory.\n",
            "  - The tool validates the path before permission approval and validates it again before writing."
        )
    }

    async fn prepare(&self, input: WriteInput) -> Result<Self::Prepared, ToolResult> {
        let existed = match validate_target(&input) {
            Ok(existed) => existed,
            Err(e) => return Err(ToolResult::error(e)),
        };

        let preview = build_preview(&input, existed);
        Ok(PreparedWrite {
            input,
            preview,
            existed,
        })
    }

    fn permission_preview(&self, prepared: &Self::Prepared) -> Option<PermissionPreview> {
        Some(PermissionPreview::Write(prepared.preview.clone()))
    }

    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        _ctx: ToolExecutionContext,
    ) -> ToolResult {
        match execute_write(&prepared).await {
            Ok(report) => ToolResult::ok(report.output).with_metadata(tool_metadata([
                ("input", serde_json::json!(prepared.input.clone())),
                (
                    "permission_preview",
                    serde_json::json!(prepared.preview.clone()),
                ),
                ("file_path", serde_json::json!(prepared.input.file_path)),
                ("existed", serde_json::json!(report.existed)),
                ("diff", serde_json::json!(report.diff)),
            ])),
            Err(e) => ToolResult::error(e).with_metadata(tool_metadata([
                ("input", serde_json::json!(prepared.input)),
                ("permission_preview", serde_json::json!(prepared.preview)),
                ("existed", serde_json::json!(prepared.existed)),
            ])),
        }
    }
}

#[derive(Debug)]
struct WriteReport {
    output: String,
    existed: bool,
    diff: String,
}

async fn execute_write(prepared: &PreparedWrite) -> Result<WriteReport, String> {
    let existed = validate_target(&prepared.input)?;
    let path = Path::new(&prepared.input.file_path);
    let parent = path
        .parent()
        .ok_or_else(|| format!("Path has no parent directory: {}", prepared.input.file_path))?;

    let _guard = FileLockService::instance().acquire(path).await;

    // 锁内读取上一版内容,这样算出的 diff 才与本次写入保持一致。
    let previous = if existed {
        fs::read_to_string(path).await.unwrap_or_default()
    } else {
        String::new()
    };

    fs::create_dir_all(parent).await.map_err(|e| {
        format!(
            "Failed to create parent directory {}: {e}",
            parent.display()
        )
    })?;

    fs::write(path, &prepared.input.content)
        .await
        .map_err(|e| format!("Failed to write file {}: {e}", prepared.input.file_path))?;

    let action = if existed { "Overwrote" } else { "Created" };
    let diff = unified_diff(
        &previous,
        &prepared.input.content,
        &prepared.input.file_path,
    );
    Ok(WriteReport {
        output: format!("{action} {}", prepared.input.file_path),
        existed,
        diff,
    })
}

fn validate_target(input: &WriteInput) -> Result<bool, String> {
    if input.file_path.trim().is_empty() {
        return Err("file_path must not be empty".to_string());
    }

    let path = Path::new(&input.file_path);
    if !path.is_absolute() {
        return Err(format!("file_path must be absolute: {}", input.file_path));
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("Path has no parent directory: {}", input.file_path))?;
    if parent.exists() && !parent.is_dir() {
        return Err(format!(
            "Parent path is not a directory: {}",
            parent.display()
        ));
    }
    if path.is_dir() {
        return Err(format!("Path is a directory: {}", input.file_path));
    }

    Ok(path.exists())
}

fn line_span_count(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        s.bytes().filter(|b| *b == b'\n').count() + 1
    }
}

fn unified_diff(old: &str, new: &str, file_path: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut formatter = diff.unified_diff();
    formatter.context_radius(3).missing_newline_hint(false);
    let file = file_path.rsplit('/').next().unwrap_or(file_path);
    formatter.header(file, file);
    let mut out: Vec<u8> = Vec::new();
    if formatter.to_writer(&mut out).is_err() {
        return String::new();
    }
    String::from_utf8(out).unwrap_or_default()
}

fn build_preview(input: &WriteInput, existed: bool) -> EditPermissionPreview {
    let action = if existed { "Overwrite" } else { "Create" };
    EditPermissionPreview {
        summary: format!("{action} {}", input.file_path),
        path: input.file_path.clone(),
        replacement_count: 1,
        diff: preview_diff_for_write(input, existed),
    }
}

fn preview_diff_for_write(input: &WriteInput, existed: bool) -> String {
    if !existed {
        return preview_diff_new_file(input);
    }
    // 已存在时 preview 阶段拿不到旧内容,只能给出"全部为新增"的最佳猜测。
    preview_diff_added_only(input)
}

fn preview_diff_new_file(input: &WriteInput) -> String {
    let mut out = String::from("@@ -0,0 +1,");
    let count = line_span_count(&input.content);
    out.push_str(&count.to_string());
    out.push_str(" @@\n");
    for line in input.content.split('\n') {
        if line.is_empty() && !input.content.ends_with('\n') {
            // 末尾不换行时最后一段空 split 不渲染。
            continue;
        }
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn preview_diff_added_only(input: &WriteInput) -> String {
    let mut out = String::from("@@ -1,");
    out.push_str(&line_span_count(&input.content).to_string());
    out.push_str(" +1,");
    out.push_str(&line_span_count(&input.content).to_string());
    out.push_str(" @@\n");
    for line in input.content.split('\n') {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!(
                "omini_write_test_{}_{}_{}",
                std::process::id(),
                line!(),
                name
            ))
            .display()
            .to_string()
    }

    #[tokio::test]
    async fn test_write_creates_file() {
        let path = temp_path("create.txt");
        let _ = fs::remove_file(&path).await;

        let input = WriteInput {
            file_path: path.clone(),
            content: "one\ntwo\n".to_string(),
        };
        let prepared = WriteTool.prepare(input).await.unwrap();
        assert!(!prepared.existed);

        let result = WriteTool
            .execute_prepared(prepared, ToolExecutionContext::test("write"))
            .await;
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn test_write_creates_missing_parent_directories() {
        let path = std::env::temp_dir()
            .join(format!(
                "omini_write_test_{}_{}_missing",
                std::process::id(),
                line!()
            ))
            .join("nested")
            .join("create.txt");
        let root = path
            .parent()
            .and_then(Path::parent)
            .expect("test path should have root")
            .to_path_buf();
        let _ = fs::remove_dir_all(&root).await;

        let input = WriteInput {
            file_path: path.display().to_string(),
            content: "created\n".to_string(),
        };
        let prepared = WriteTool.prepare(input).await.unwrap();
        assert!(!prepared.existed);
        assert!(!path.parent().unwrap().exists());

        let result = WriteTool
            .execute_prepared(prepared, ToolExecutionContext::test("write"))
            .await;
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "created\n");

        let _ = fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn test_write_rejects_relative_path() {
        let input = WriteInput {
            file_path: "relative.txt".to_string(),
            content: "content".to_string(),
        };
        let err = WriteTool.prepare(input).await.unwrap_err();
        assert!(err.output.contains("file_path must be absolute"));
    }

    #[tokio::test]
    async fn test_write_overwrites_file() {
        let path = temp_path("overwrite.txt");
        fs::write(&path, "old\n").await.unwrap();

        let input = WriteInput {
            file_path: path.clone(),
            content: "new\n".to_string(),
        };
        let prepared = WriteTool.prepare(input).await.unwrap();
        assert!(prepared.existed);

        let result = WriteTool
            .execute_prepared(prepared, ToolExecutionContext::test("write"))
            .await;
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "new\n");

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn write_acquires_file_lock_for_concurrent_writes() {
        let path = temp_path("lock.txt");
        let _ = fs::remove_file(&path).await;

        // 外部预先拿锁,WriteTool 调用应被阻塞。
        let _external = FileLockService::instance().acquire(Path::new(&path)).await;

        let path_for_tool = path.clone();
        let tool_task = tokio::spawn(async move {
            let input = WriteInput {
                file_path: path_for_tool,
                content: "hello\n".to_string(),
            };
            let prepared = WriteTool.prepare(input).await.unwrap();
            WriteTool
                .execute_prepared(prepared, ToolExecutionContext::test("write"))
                .await
        });

        // 此时 WriteTool 还没完成。
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!tool_task.is_finished(), "tool should be blocked by lock");

        // 释放锁,让 tool 跑完。
        drop(_external);
        let result = tokio::time::timeout(Duration::from_secs(1), tool_task)
            .await
            .expect("tool should finish after lock release")
            .unwrap();
        assert!(!result.is_error, "{}", result.output);

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn write_emits_unified_diff_in_metadata() {
        let path = temp_path("diff.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").await.unwrap();
        let input = WriteInput {
            file_path: path.clone(),
            content: "alpha\nBETA\ngamma\ndelta\n".to_string(),
        };
        let prepared = WriteTool.prepare(input).await.unwrap();
        let result = WriteTool
            .execute_prepared(prepared, ToolExecutionContext::test("write"))
            .await;
        assert!(!result.is_error, "{}", result.output);
        let metadata = result.metadata.expect("metadata should be set");
        let diff = metadata
            .get("diff")
            .and_then(|v| v.as_str())
            .expect("diff in metadata");
        let file_name = std::path::Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap();
        assert!(diff.contains(&format!("--- {file_name}")));
        assert!(diff.contains(&format!("+++ {file_name}")));
        assert!(diff.contains("-beta"));
        assert!(diff.contains("+BETA"));
        assert!(diff.contains("+delta"));
        let _ = fs::remove_file(path).await;
    }
}
