use super::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use omini_domain::events::{PermissionPreview, ReadPermissionPreview};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::fs;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadInput {
    /// The absolute path to the file or directory to read
    pub file_path: String,
    /// The line number to start reading from (1-indexed, default: 1)
    pub offset: Option<usize>,
    /// Maximum number of lines to read (default: 2000)
    pub limit: Option<usize>,
}

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    type Input = ReadInput;
    type Prepared = ReadInput;

    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        concat!(
            "Read a file or directory from the local filesystem.\n",
            "\n",
            "Usage:\n",
            "  file_path  Absolute path to the file or directory. Relative paths are rejected.\n",
            "  offset     Line number to start from (1-indexed, default: 1).\n",
            "  limit      Max lines to return (default: 2000).\n",
            "\n",
            "Rules:\n",
            "  - file_path must be absolute; do not pass relative paths.\n",
            "\n",
            "Text files: each line is returned as `<line>: <content>`.\n",
            "Directories: entries listed with `/` suffix for subdirectories.\n",
            "Binary files: returns file size instead of content."
        )
    }

    async fn prepare(&self, input: ReadInput) -> Result<Self::Prepared, ToolResult> {
        let path = std::path::Path::new(&input.file_path);
        if !path.is_absolute() {
            return Err(ToolResult::error(format!(
                "file_path must be absolute: {}",
                input.file_path
            )));
        }
        if !path.exists() {
            return Err(ToolResult::error(format!(
                "Path does not exist: {}",
                input.file_path
            )));
        }
        Ok(input)
    }

    fn permission_preview(&self, prepared: &Self::Prepared) -> Option<PermissionPreview> {
        Some(PermissionPreview::Read(ReadPermissionPreview {
            file_path: prepared.file_path.clone(),
        }))
    }

    async fn execute_prepared(
        &self,
        input: Self::Prepared,
        _ctx: ToolExecutionContext,
    ) -> ToolResult {
        let path = std::path::Path::new(&input.file_path);

        if !path.is_absolute() {
            return ToolResult::error(format!("file_path must be absolute: {}", input.file_path));
        }

        // Check if path exists
        if !path.exists() {
            return ToolResult::error(format!("Path does not exist: {}", input.file_path));
        }

        // Directory listing
        if path.is_dir() {
            return read_directory(path).await;
        }

        // File reading
        read_file(path, input.offset, input.limit).await
    }
}

async fn read_directory(path: &std::path::Path) -> ToolResult {
    let mut entries = match fs::read_dir(path).await {
        Ok(entries) => entries,
        Err(e) => return ToolResult::error(format!("Failed to read directory: {e}")),
    };

    let mut output = String::new();
    let mut entry_list: Vec<String> = Vec::new();

    while let Some(entry) = entries.next_entry().await.transpose() {
        match entry {
            Ok(entry) => {
                let name = entry.file_name().to_string_lossy().to_string();
                let suffix = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                    "/"
                } else {
                    ""
                };
                entry_list.push(format!("{name}{suffix}"));
            }
            Err(e) => {
                entry_list.push(format!("<error reading entry: {e}>"));
            }
        }
    }

    entry_list.sort();
    for entry in entry_list {
        output.push_str(&entry);
        output.push('\n');
    }

    if output.is_empty() {
        output = "(empty directory)".to_string();
    }

    ToolResult::ok(output.trim().to_string())
}

async fn read_file(
    path: &std::path::Path,
    offset: Option<usize>,
    limit: Option<usize>,
) -> ToolResult {
    // Read file content
    let content = match fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => {
            // Check if it's a binary file
            if e.kind() == std::io::ErrorKind::InvalidData {
                let metadata = match fs::metadata(path).await {
                    Ok(m) => m,
                    Err(e2) => return ToolResult::error(format!("Cannot read file: {e2}")),
                };
                return ToolResult::ok(format!("(binary file, {} bytes)", metadata.len()));
            }
            // Permission denied or other error
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                return ToolResult::error(format!("Permission denied: {}", path.display()));
            }
            return ToolResult::error(format!("Failed to read file: {e}"));
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let start = offset.unwrap_or(1).max(1);
    let start_idx = start - 1; // convert to 0-indexed

    if start_idx >= total_lines {
        return ToolResult::ok(format!(
            "(file has {total_lines} lines, starting at line {start} is past end)"
        ));
    }

    let limit = limit.unwrap_or(2000);
    let end_idx = (start_idx + limit).min(total_lines);

    let mut output = String::new();
    for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
        output.push_str(&format!("{}: {}\n", start + i, line));
    }

    // Append a status line if file was truncated
    if end_idx < total_lines {
        output.push_str(&format!(
            "(... {}/{} lines shown, use higher offset to continue)\n",
            end_idx - start_idx,
            total_lines
        ));
    }

    ToolResult::ok(output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_rejects_relative_path() {
        let input = ReadInput {
            file_path: "relative.txt".to_string(),
            offset: None,
            limit: None,
        };
        let err = ReadTool.prepare(input).await.unwrap_err();
        assert!(err.output.contains("file_path must be absolute"));
    }
}
