use super::{Tool, ToolExecutionContext, ToolResult, tool_metadata};
use async_trait::async_trait;
use omini_domain::events::{EditPermissionPreview, PermissionPreview};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct EditInput {
    /// The absolute path to the file to edit.
    pub file_path: String,
    /// Exact text to replace. Must match the file content exactly.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace every occurrence. Defaults to false; when false, old_string must be unique.
    #[serde(default)]
    pub replace_all: Option<bool>,
}

pub struct EditTool;

#[derive(Debug, Clone, Serialize)]
pub struct EditMatch {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug)]
pub struct PreparedEdit {
    input: EditInput,
    preview: EditPermissionPreview,
    matches: Vec<EditMatch>,
}

#[async_trait]
impl Tool for EditTool {
    type Input = EditInput;
    type Prepared = PreparedEdit;

    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        concat!(
            "Edit a text file by exact string replacement.\n",
            "\n",
            "Use this for normal file edits when replacing a specific block of text.\n",
            "\n",
            "Input:\n",
            "  file_path    Absolute path to the file.\n",
            "  old_string   Exact text to replace, copied from `read` output without line numbers.\n",
            "  new_string   Replacement text.\n",
            "  replace_all  Optional bool. Defaults to false.\n",
            "\n",
            "Rules:\n",
            "  - file_path must be absolute; do not pass relative paths.\n",
            "  - The file must already exist and be valid UTF-8 text.\n",
            "  - old_string must not be empty and must differ from new_string.\n",
            "  - When replace_all is false, old_string must match exactly once.\n",
            "  - When replace_all is true, every exact occurrence is replaced and at least one must exist.\n",
            "  - The tool validates the real file before permission approval and validates it again before writing."
        )
    }

    async fn prepare(&self, input: EditInput) -> Result<Self::Prepared, ToolResult> {
        let plan = match plan_edit(&input).await {
            Ok(plan) => plan,
            Err(e) => return Err(ToolResult::error(e)),
        };

        let preview = build_preview(&input, &plan.matches);
        Ok(PreparedEdit {
            input,
            preview,
            matches: plan.matches,
        })
    }

    fn permission_preview(&self, prepared: &Self::Prepared) -> Option<PermissionPreview> {
        Some(PermissionPreview::Edit(prepared.preview.clone()))
    }

    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        _ctx: ToolExecutionContext,
    ) -> ToolResult {
        match execute_edit(&prepared).await {
            Ok(report) => ToolResult::ok(report.output).with_metadata(tool_metadata([
                ("input", serde_json::json!(prepared.input.clone())),
                (
                    "permission_preview",
                    serde_json::json!(prepared.preview.clone()),
                ),
                (
                    "prepared_matches",
                    serde_json::json!(prepared.matches.clone()),
                ),
                ("matches", serde_json::json!(report.matches)),
                (
                    "replacement_count",
                    serde_json::json!(report.replacement_count),
                ),
                ("file_path", serde_json::json!(prepared.input.file_path)),
            ])),
            Err(e) => ToolResult::error(e).with_metadata(tool_metadata([
                ("input", serde_json::json!(prepared.input)),
                ("permission_preview", serde_json::json!(prepared.preview)),
                ("prepared_matches", serde_json::json!(prepared.matches)),
                ("matches", serde_json::json!([])),
                ("replacement_count", serde_json::json!(0)),
            ])),
        }
    }
}

#[derive(Debug)]
struct EditPlan {
    content: String,
    matches: Vec<EditMatch>,
}

#[derive(Debug)]
struct EditReport {
    output: String,
    matches: Vec<EditMatch>,
    replacement_count: usize,
}

async fn execute_edit(prepared: &PreparedEdit) -> Result<EditReport, String> {
    let plan = plan_edit(&prepared.input).await?;

    let new_content = if prepared.input.replace_all.unwrap_or(false) {
        plan.content
            .replace(&prepared.input.old_string, &prepared.input.new_string)
    } else {
        let m = plan
            .matches
            .first()
            .expect("single-edit plan should have one match");
        let mut new_content = plan.content;
        new_content.replace_range(m.start_byte..m.end_byte, &prepared.input.new_string);
        new_content
    };

    fs::write(&prepared.input.file_path, new_content)
        .await
        .map_err(|e| format!("Failed to write file {}: {e}", prepared.input.file_path))?;

    let replacement_count = plan.matches.len();
    Ok(EditReport {
        output: format!(
            "edit: Replaced {replacement_count} occurrence(s) in {}",
            prepared.input.file_path
        ),
        matches: plan.matches,
        replacement_count,
    })
}

async fn plan_edit(input: &EditInput) -> Result<EditPlan, String> {
    validate_input(input)?;

    let path = Path::new(&input.file_path);
    if !path.exists() {
        return Err(format!("Path does not exist: {}", input.file_path));
    }
    if !path.is_file() {
        return Err(format!("Path is not a file: {}", input.file_path));
    }

    let content = fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read file {}: {e}", input.file_path))?;

    let matches = find_matches(&content, &input.old_string);
    if matches.is_empty() {
        return Err(format!("old_string not found in {}", input.file_path));
    }

    if !input.replace_all.unwrap_or(false) && matches.len() > 1 {
        return Err(format!(
            "Found multiple matches ({}) for old_string in {}. Set replace_all=true or provide a more specific old_string.",
            matches.len(),
            input.file_path
        ));
    }

    Ok(EditPlan { content, matches })
}

fn validate_input(input: &EditInput) -> Result<(), String> {
    if input.file_path.trim().is_empty() {
        return Err("file_path must not be empty".to_string());
    }
    if !Path::new(&input.file_path).is_absolute() {
        return Err(format!("file_path must be absolute: {}", input.file_path));
    }
    if input.old_string.is_empty() {
        return Err("old_string must not be empty".to_string());
    }
    if input.old_string == input.new_string {
        return Err("old_string and new_string must be different".to_string());
    }
    Ok(())
}

fn find_matches(content: &str, needle: &str) -> Vec<EditMatch> {
    content
        .match_indices(needle)
        .map(|(start_byte, matched)| {
            let end_byte = start_byte + matched.len();
            EditMatch {
                start_byte,
                end_byte,
                start_line: line_number_at_byte(content, start_byte),
                end_line: line_number_at_byte(content, end_byte),
            }
        })
        .collect()
}

fn line_number_at_byte(content: &str, byte_idx: usize) -> usize {
    content[..byte_idx].bytes().filter(|b| *b == b'\n').count() + 1
}

fn line_span_count(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        s.bytes().filter(|b| *b == b'\n').count() + 1
    }
}

fn build_preview(input: &EditInput, matches: &[EditMatch]) -> EditPermissionPreview {
    EditPermissionPreview {
        summary: format!(
            "Edit {} occurrence(s) in {}",
            matches.len(),
            input.file_path
        ),
        path: input.file_path.clone(),
        replacement_count: matches.len(),
        replace_all: input.replace_all.unwrap_or(false),
        start_lines: matches.iter().map(|m| m.start_line).collect(),
        added_lines: line_span_count(&input.new_string) * matches.len(),
        removed_lines: line_span_count(&input.old_string) * matches.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!(
                "omini_edit_test_{}_{}_{}",
                std::process::id(),
                line!(),
                name
            ))
            .display()
            .to_string()
    }

    #[tokio::test]
    async fn test_edit_replaces_unique_match() {
        let path = temp_path("unique.txt");
        fs::write(&path, "one\ntwo\nthree\n").await.unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "two\n".to_string(),
            new_string: "TWO\n".to_string(),
            replace_all: None,
        };
        let prepared = EditTool.prepare(input).await.unwrap();
        assert_eq!(prepared.matches[0].start_line, 2);

        let result = EditTool
            .execute_prepared(prepared, ToolExecutionContext::test("edit"))
            .await;
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(
            fs::read_to_string(&path).await.unwrap(),
            "one\nTWO\nthree\n"
        );

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn test_edit_rejects_multiple_matches_without_replace_all() {
        let path = temp_path("multiple.txt");
        fs::write(&path, "same\nsame\n").await.unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "same".to_string(),
            new_string: "other".to_string(),
            replace_all: None,
        };
        let err = EditTool.prepare(input).await.unwrap_err();
        assert!(err.output.contains("Found multiple matches"));

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn test_edit_rejects_relative_path() {
        let input = EditInput {
            file_path: "relative.txt".to_string(),
            old_string: "old".to_string(),
            new_string: "new".to_string(),
            replace_all: None,
        };
        let err = EditTool.prepare(input).await.unwrap_err();
        assert!(err.output.contains("file_path must be absolute"));
    }

    #[tokio::test]
    async fn test_edit_revalidates_before_write() {
        let path = temp_path("changed.txt");
        fs::write(&path, "before\n").await.unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "before".to_string(),
            new_string: "after".to_string(),
            replace_all: None,
        };
        let prepared = EditTool.prepare(input).await.unwrap();
        fs::write(&path, "changed\n").await.unwrap();

        let result = EditTool
            .execute_prepared(prepared, ToolExecutionContext::test("edit"))
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("old_string not found"));
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "changed\n");

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let path = temp_path("all.txt");
        fs::write(&path, "x y x").await.unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "x".to_string(),
            new_string: "z".to_string(),
            replace_all: Some(true),
        };
        let prepared = EditTool.prepare(input).await.unwrap();
        assert_eq!(prepared.matches.len(), 2);

        let result = EditTool
            .execute_prepared(prepared, ToolExecutionContext::test("edit"))
            .await;
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(fs::read_to_string(&path).await.unwrap(), "z y z");

        let _ = fs::remove_file(path).await;
    }
}
