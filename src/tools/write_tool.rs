use super::{Tool, ToolExecutionContext, ToolResult, tool_metadata};
use crate::types::events::{EditPermissionPreview, PermissionPreview};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
            "  - The parent directory must already exist.\n",
            "  - The target path must not be a directory.\n",
            "  - The tool validates the path before permission approval and validates it again before writing."
        )
    }

    async fn prepare(&self, input: WriteInput) -> Result<Self::Prepared, ToolResult> {
        let existed = match validate_target(&input).await {
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
                ("added_lines", serde_json::json!(report.added_lines)),
                ("existed", serde_json::json!(report.existed)),
            ])),
            Err(e) => ToolResult::error(e).with_metadata(tool_metadata([
                ("input", serde_json::json!(prepared.input)),
                ("permission_preview", serde_json::json!(prepared.preview)),
                ("added_lines", serde_json::json!(0)),
                ("existed", serde_json::json!(prepared.existed)),
            ])),
        }
    }
}

#[derive(Debug)]
struct WriteReport {
    output: String,
    added_lines: usize,
    existed: bool,
}

async fn execute_write(prepared: &PreparedWrite) -> Result<WriteReport, String> {
    let existed = validate_target(&prepared.input).await?;

    fs::write(&prepared.input.file_path, &prepared.input.content)
        .await
        .map_err(|e| format!("Failed to write file {}: {e}", prepared.input.file_path))?;

    let action = if existed { "Overwrote" } else { "Created" };
    let added_lines = line_span_count(&prepared.input.content);
    Ok(WriteReport {
        output: format!("{action} {}", prepared.input.file_path),
        added_lines,
        existed,
    })
}

async fn validate_target(input: &WriteInput) -> Result<bool, String> {
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
    if !parent.exists() {
        return Err(format!(
            "Parent directory does not exist: {}",
            parent.display()
        ));
    }
    if !parent.is_dir() {
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

fn build_preview(input: &WriteInput, existed: bool) -> EditPermissionPreview {
    let added_lines = line_span_count(&input.content);
    let action = if existed { "Overwrite" } else { "Create" };
    EditPermissionPreview {
        summary: format!("{action} {}", input.file_path),
        path: input.file_path.clone(),
        replacement_count: 1,
        replace_all: false,
        start_lines: vec![1],
        added_lines,
        removed_lines: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(prepared.preview.added_lines, 3);
        assert!(!prepared.existed);

        let result = WriteTool
            .execute_prepared(prepared, ToolExecutionContext::test("write"))
            .await;
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "one\ntwo\n");

        let _ = fs::remove_file(path).await;
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
}
