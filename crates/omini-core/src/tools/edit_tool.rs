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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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
            "  - The tool applies a small chain of forgiving matchers (trim, indent, escape) and re-validates the real file before writing."
        )
    }

    async fn prepare(&self, input: EditInput) -> Result<Self::Prepared, ToolResult> {
        let plan = match plan_edit(&input).await {
            Ok(plan) => plan,
            Err(e) => return Err(ToolResult::error(e)),
        };

        let preview = build_preview(&input, &plan.matches, &plan.diff);
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
                // plan 阶段算出的 match;与下面的 matches 在 replace_all=true 时
                // 可能因为 execute 期间出现新匹配而不同。
                (
                    "prepared_matches",
                    serde_json::json!(prepared.matches.clone()),
                ),
                // execute 阶段最终采用的 match 列表。
                ("matches", serde_json::json!(report.matches.clone())),
                (
                    "replacement_count",
                    serde_json::json!(report.replacement_count),
                ),
                ("file_path", serde_json::json!(prepared.input.file_path)),
                ("diff", serde_json::json!(report.diff)),
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
    matches: Vec<EditMatch>,
    /// unified_diff(content, new_content),供 EditPermissionPreview 渲染。
    diff: String,
}

#[derive(Debug)]
struct EditReport {
    output: String,
    matches: Vec<EditMatch>,
    replacement_count: usize,
    diff: String,
}

async fn execute_edit(prepared: &PreparedEdit) -> Result<EditReport, String> {
    let path = Path::new(&prepared.input.file_path);
    let _guard = FileLockService::instance().acquire(path).await;

    let content = fs::read_to_string(path)
        .await
        .map_err(|e| format!("Failed to read file {}: {e}", prepared.input.file_path))?;

    let line_ending = detect_line_ending(&content);
    let normalized_new = convert_to_line_ending(
        &normalize_line_endings(&prepared.input.new_string),
        line_ending,
    );

    let replace_all = prepared.input.replace_all.unwrap_or(false);
    // 快路径:prepared.matches 的字节位置和 old_string 完全一致,直接复用。
    let matches = if !replace_all
        && prepared.matches.len() == 1
        && let Some(m) = prepared.matches.first()
        && content.get(m.start_byte..m.end_byte) == Some(&prepared.input.old_string)
    {
        prepared.matches.clone()
    } else {
        // 慢路径:重新跑 find_matches,处理文件被外部改掉等场景。
        let normalized_old = convert_to_line_ending(
            &normalize_line_endings(&prepared.input.old_string),
            line_ending,
        );
        let matches = find_matches(&content, &normalized_old, &prepared.input.old_string);
        if matches.is_empty() {
            return Err(format!(
                "old_string not found in {}",
                prepared.input.file_path
            ));
        }
        if !replace_all && matches.len() > 1 {
            return Err(format!(
                "Found multiple matches ({}) for old_string in {}. Set replace_all=true or provide a more specific old_string.",
                matches.len(),
                prepared.input.file_path
            ));
        }
        matches
    };

    let new_content = apply_matches(&content, &matches, &normalized_new);
    let diff = unified_diff(&content, &new_content, &prepared.input.file_path);

    fs::write(path, &new_content)
        .await
        .map_err(|e| format!("Failed to write file {}: {e}", prepared.input.file_path))?;

    let replacement_count = matches.len();
    Ok(EditReport {
        output: format!(
            "edit: Replaced {replacement_count} occurrence(s) in {}",
            prepared.input.file_path
        ),
        matches,
        replacement_count,
        diff,
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

    let line_ending = detect_line_ending(&content);
    let normalized_old =
        convert_to_line_ending(&normalize_line_endings(&input.old_string), line_ending);
    let normalized_new =
        convert_to_line_ending(&normalize_line_endings(&input.new_string), line_ending);

    let replace_all = input.replace_all.unwrap_or(false);
    let matches = find_matches(&content, &normalized_old, &input.old_string);
    if matches.is_empty() {
        return Err(format!("old_string not found in {}", input.file_path));
    }
    if !replace_all && matches.len() > 1 {
        return Err(format!(
            "Found multiple matches ({}) for old_string in {}. Set replace_all=true or provide a more specific old_string.",
            matches.len(),
            input.file_path
        ));
    }

    // 与 execute_edit 共享 unified_diff,文件未变时 byte-equal。
    let new_content = apply_matches(&content, &matches, &normalized_new);
    let diff = unified_diff(&content, &new_content, &input.file_path);

    Ok(EditPlan { matches, diff })
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

fn normalize_line_endings(text: &str) -> String {
    // CRLF → LF,其他情况原样输出。
    if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    }
}

fn detect_line_ending(text: &str) -> LineEnding {
    if text.contains("\r\n") {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

fn convert_to_line_ending(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => text.to_string(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

/// 单次容错匹配尝试:实际定位到的字节范围加上命中的策略名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacerCandidate {
    pub search: String,
    pub strategy: &'static str,
}

pub type Replacer = fn(&str, &str) -> Vec<ReplacerCandidate>;

/// opencode 链中 4 个 ROI 最高的策略。顺序很关键:
/// 先试 `simple`,只有它的结果不唯一时才向后回退。
const ACTIVE_REPLACERS: &[(&str, Replacer)] = &[
    ("simple", simple_replacer),
    ("line_trim", line_trim_replacer),
    ("indentation_flexible", indentation_flexible_replacer),
    ("escape_normalized", escape_normalized_replacer),
];

fn simple_replacer(_content: &str, find: &str) -> Vec<ReplacerCandidate> {
    vec![ReplacerCandidate {
        search: find.to_string(),
        strategy: "simple",
    }]
}

fn line_trim_replacer(content: &str, find: &str) -> Vec<ReplacerCandidate> {
    let search_lines: Vec<&str> = find.split('\n').collect();
    if search_lines.is_empty() {
        return Vec::new();
    }
    let original_lines: Vec<&str> = content.split('\n').collect();
    let mut candidates = Vec::new();
    let mut i = 0;
    while i + search_lines.len() <= original_lines.len() {
        let mut ok = true;
        for k in 0..search_lines.len() {
            if original_lines[i + k].trim() != search_lines[k].trim() {
                ok = false;
                break;
            }
        }
        if ok {
            let start_byte = original_lines[..i]
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>();
            let end_byte = start_byte
                + original_lines[i..i + search_lines.len()]
                    .iter()
                    .map(|line| line.len())
                    .sum::<usize>()
                + search_lines.len().saturating_sub(1);
            let end_byte = end_byte.min(content.len()).max(start_byte);
            let search = content[start_byte..end_byte].to_string();
            if !search.is_empty() {
                candidates.push(ReplacerCandidate {
                    search,
                    strategy: "line_trim",
                });
            }
            i += search_lines.len();
        } else {
            i += 1;
        }
    }
    candidates
}

fn indentation_flexible_replacer(content: &str, find: &str) -> Vec<ReplacerCandidate> {
    let search_lines: Vec<&str> = find.split('\n').collect();
    if search_lines.is_empty() {
        return Vec::new();
    }
    let min_indent = search_lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.bytes()
                .take_while(|b| *b == b' ' || *b == b'\t')
                .count()
        })
        .min()
        .unwrap_or(0);
    if min_indent == 0 {
        return Vec::new();
    }
    let stripped: Vec<&str> = search_lines
        .iter()
        .map(|line| line.get(min_indent..).unwrap_or(line))
        .collect();
    let original_lines: Vec<&str> = content.split('\n').collect();
    let mut candidates = Vec::new();
    let mut i = 0;
    while i + stripped.len() <= original_lines.len() {
        let block_indent = original_lines[i..i + stripped.len()]
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                line.bytes()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .count()
            })
            .min()
            .unwrap_or(0);
        let mut ok = true;
        for k in 0..stripped.len() {
            let candidate_line = if block_indent >= min_indent {
                original_lines[i + k]
                    .get(block_indent..)
                    .unwrap_or(original_lines[i + k])
            } else {
                original_lines[i + k]
            };
            if candidate_line.trim() != stripped[k].trim() {
                ok = false;
                break;
            }
        }
        if ok {
            let start_byte = original_lines[..i]
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>();
            let end_byte = start_byte
                + original_lines[i..i + stripped.len()]
                    .iter()
                    .map(|line| line.len())
                    .sum::<usize>()
                + stripped.len().saturating_sub(1);
            let end_byte = end_byte.min(content.len()).max(start_byte);
            let search = content[start_byte..end_byte].to_string();
            if !search.is_empty() {
                candidates.push(ReplacerCandidate {
                    search,
                    strategy: "indentation_flexible",
                });
            }
            i += stripped.len();
        } else {
            i += 1;
        }
    }
    candidates
}

fn escape_normalized_replacer(content: &str, find: &str) -> Vec<ReplacerCandidate> {
    let unescaped = unescape_literal(find);
    if unescaped == find {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    if content.contains(&unescaped) {
        candidates.push(ReplacerCandidate {
            search: unescaped.clone(),
            strategy: "escape_normalized",
        });
    }
    // per-line 回退:LLM 给的 block 可能混着转义换行和真实换行。
    let original_lines: Vec<&str> = content.split('\n').collect();
    let find_lines: Vec<&str> = unescaped.split('\n').collect();
    if find_lines.len() >= 2 {
        let mut i = 0;
        while i + find_lines.len() <= original_lines.len() {
            let mut ok = true;
            for k in 0..find_lines.len() {
                if original_lines[i + k] != find_lines[k] {
                    ok = false;
                    break;
                }
            }
            if ok {
                let start_byte = original_lines[..i]
                    .iter()
                    .map(|line| line.len() + 1)
                    .sum::<usize>();
                let end_byte = start_byte
                    + original_lines[i..i + find_lines.len()]
                        .iter()
                        .map(|line| line.len())
                        .sum::<usize>()
                    + find_lines.len().saturating_sub(1);
                let end_byte = end_byte.min(content.len()).max(start_byte);
                let search = content[start_byte..end_byte].to_string();
                if !search.is_empty() {
                    candidates.push(ReplacerCandidate {
                        search,
                        strategy: "escape_normalized",
                    });
                }
                i += find_lines.len();
            } else {
                i += 1;
            }
        }
    }
    candidates
}

fn unescape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceError {
    EmptyOldString,
    SameAsNew,
    NotFound,
    MultipleMatches,
    Disproportionate,
}

pub fn replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, ReplaceError> {
    if old.is_empty() {
        return Err(ReplaceError::EmptyOldString);
    }
    if old == new {
        return Err(ReplaceError::SameAsNew);
    }
    for (name, replacer) in ACTIVE_REPLACERS {
        for candidate in replacer(content, old) {
            if is_disproportionate_match(&candidate.search, old) {
                continue;
            }
            if !content.contains(&candidate.search) {
                continue;
            }
            if replace_all {
                return Ok(content.replace(&candidate.search, new));
            }
            if content.matches(&candidate.search).count() != 1 {
                continue;
            }
            if let Some(_idx) = content.find(&candidate.search) {
                let _ = name;
                return Ok(content.replacen(&candidate.search, new, 1));
            }
        }
    }
    if !replace_all && content.matches(old).count() > 1 {
        return Err(ReplaceError::MultipleMatches);
    }
    Err(ReplaceError::NotFound)
}

fn is_disproportionate_match(search: &str, old: &str) -> bool {
    let old_lines = count_lines(old);
    let search_lines = count_lines(search);
    if search_lines >= old_lines.saturating_add(3).max(old_lines.saturating_mul(2)) {
        return true;
    }
    if old_lines == 1 {
        return false;
    }
    let max_len = old
        .trim()
        .len()
        .saturating_add(500)
        .max(old.trim().len() * 4);
    search.trim().len() > max_len
}

fn count_lines(s: &str) -> usize {
    if s.is_empty() {
        0
    } else {
        s.bytes().filter(|b| *b == b'\n').count() + 1
    }
}

fn collect_candidates(
    content: &str,
    original_old: &str,
    normalized_old: &str,
) -> Vec<ReplacerCandidate> {
    let mut out = Vec::new();
    for (name, replacer) in ACTIVE_REPLACERS {
        let mut produced = replacer(content, normalized_old);
        for candidate in &mut produced {
            // 注册表里 name 与 candidate.strategy 都会标注策略名,
            // 两者一致,这里保留 candidate 自己写的版本即可。
            let _ = name;
            if candidate.strategy == "simple" && candidate.search == original_old {
                // 精确匹配:策略名保持 simple。
            }
            if !out
                .iter()
                .any(|c: &ReplacerCandidate| c.search == candidate.search)
            {
                out.push(candidate.clone());
            }
        }
    }
    out
}

fn find_matches(content: &str, normalized_old: &str, original_old: &str) -> Vec<EditMatch> {
    // 1. 先尝试用 original_old 精确匹配,保证常见场景下行号语义与历史一致。
    if let Some(matches) = exact_matches(content, original_old)
        && !matches.is_empty()
    {
        return matches;
    }
    // 2. 退一步用 normalized_old 精确匹配(覆盖行尾差异场景)。
    if let Some(matches) = exact_matches(content, normalized_old)
        && !matches.is_empty()
    {
        return matches;
    }
    // 3. 走容错链:line_trim / indent_flexible / escape_normalized。
    // 用 disproportionate 守卫过滤掉明显过大的候选,行为与历史 resolve_matches 一致。
    let candidates = collect_candidates(content, original_old, normalized_old);
    for candidate in candidates {
        if is_disproportionate_match(&candidate.search, original_old) {
            continue;
        }
        if let Some(matches) = exact_matches(content, &candidate.search)
            && !matches.is_empty()
        {
            return matches;
        }
    }
    Vec::new()
}

fn exact_matches(content: &str, needle: &str) -> Option<Vec<EditMatch>> {
    if needle.is_empty() {
        return Some(Vec::new());
    }
    let matches: Vec<EditMatch> = content
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
        .collect();
    Some(matches)
}

fn apply_matches(content: &str, matches: &[EditMatch], replacement: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0usize;
    for m in matches {
        out.push_str(&content[cursor..m.start_byte]);
        out.push_str(replacement);
        cursor = m.end_byte;
    }
    out.push_str(&content[cursor..]);
    out
}

fn line_number_at_byte(content: &str, byte_idx: usize) -> usize {
    content[..byte_idx].bytes().filter(|b| *b == b'\n').count() + 1
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

fn build_preview(input: &EditInput, matches: &[EditMatch], diff: &str) -> EditPermissionPreview {
    EditPermissionPreview {
        summary: format!(
            "Edit {} occurrence(s) in {}",
            matches.len(),
            input.file_path
        ),
        path: input.file_path.clone(),
        replacement_count: matches.len(),
        diff: diff.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolExecutionContext;

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

    #[test]
    fn replace_simple_matches_exact_string() {
        let content = "hello world\nnext line\n";
        let result = replace(content, "hello world", "HELLO", false).unwrap();
        assert_eq!(result, "HELLO\nnext line\n");
    }

    #[test]
    fn replace_line_trim_handles_trailing_whitespace() {
        let content = "alpha   \nbeta\n";
        // line_trim_replacer 会定位到磁盘上的真实子串(含尾随空白),
        // replace 操作就把它原样替换掉 —— 与 opencode 的 replace() 语义一致。
        let result = replace(content, "alpha", "ALPHA", false).unwrap();
        assert_eq!(result, "ALPHA   \nbeta\n");
    }

    #[test]
    fn replace_line_trim_falls_back_to_simple() {
        let content = "alpha\nbeta\n";
        // 第一个 replacer 返回 `alpha`;第二个返回原 `alpha` 候选。
        // 两条路径任一正确即可。
        let result = replace(content, "alpha\nbeta", "ALPHA\nBETA", false).unwrap();
        assert_eq!(result, "ALPHA\nBETA\n");
    }

    #[test]
    fn replace_indentation_flexible_handles_indent_diff() {
        let content = "    fn main() { println!(\"hi\"); }\n";
        // indent-flexible 候选就是磁盘上的真实子串,所以替换结果保留原缩进。
        let result = replace(
            content,
            "fn main() { println!(\"hi\"); }",
            "fn main() {}",
            false,
        )
        .unwrap();
        assert_eq!(result, "    fn main() {}\n");
    }

    #[test]
    fn replace_escape_normalized_handles_literal_backslash_n() {
        // 文件里是真实换行,而 LLM 传了字面 `\n` 进来。
        let content = "line1\nline2\n";
        let result = replace(content, "line1\\nline2", "merged", false).unwrap();
        assert_eq!(result, "merged\n");
    }

    #[test]
    fn replace_rejects_empty_old_string() {
        assert_eq!(
            replace("abc", "", "x", false).unwrap_err(),
            ReplaceError::EmptyOldString
        );
    }

    #[test]
    fn replace_rejects_same_as_new() {
        assert_eq!(
            replace("abc", "abc", "abc", false).unwrap_err(),
            ReplaceError::SameAsNew
        );
    }

    #[test]
    fn replace_replaces_all_when_replace_all_true() {
        let content = "x x x\n";
        let result = replace(content, "x", "y", true).unwrap();
        assert_eq!(result, "y y y\n");
    }

    #[test]
    fn replace_errors_when_multiple_matches_and_no_replace_all() {
        let content = "a a a\n";
        let err = replace(content, "a", "b", false).unwrap_err();
        assert_eq!(err, ReplaceError::MultipleMatches);
    }

    #[test]
    fn replace_normalizes_crlf_line_endings() {
        let content = "alpha\r\nbeta\r\n";
        let result = replace(content, "alpha", "ALPHA", false).unwrap();
        assert_eq!(result, "ALPHA\r\nbeta\r\n");
    }

    #[tokio::test]
    async fn preview_diff_includes_context_lines_for_partial_replacement() {
        // 把多行 old_string 中的某一行换成新行,preview 阶段就要带 context 行,
        // 这样 TUI 解析后才能正确触发开头/结尾省略、≥3 行折叠等规则,避免权限面板
        // 把所有行都画成 +/-。
        let path = temp_path("preview.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").await.unwrap();
        let input = EditInput {
            file_path: path,
            old_string: "alpha\nbeta\ngamma".to_string(),
            new_string: "alpha\nBETA\ngamma".to_string(),
            replace_all: None,
        };
        let prepared = EditTool.prepare(input).await.unwrap();
        let preview_diff = &prepared.preview.diff;
        // preview diff 必须含 context 行(前缀 ' '),以及 +/- 行;
        // 如果还像老实现那样只产出 +/- 就没有 context,header +N/-M 也会虚高。
        let mut context_count = 0;
        let mut add_count = 0;
        let mut del_count = 0;
        for line in preview_diff.lines() {
            if line.starts_with("@@") || line.starts_with("---") || line.starts_with("+++") {
                continue;
            }
            if line.starts_with(' ') {
                context_count += 1;
            } else if line.starts_with('+') {
                add_count += 1;
            } else if line.starts_with('-') {
                del_count += 1;
            }
        }
        assert!(
            context_count > 0,
            "preview diff 应包含 context 行,实际:\n{preview_diff}"
        );
        assert_eq!(add_count, 1, "preview diff 应有 1 行 +: {preview_diff}");
        assert_eq!(del_count, 1, "preview diff 应有 1 行 -: {preview_diff}");
    }

    #[test]
    fn normalize_line_endings_collapses_crlf() {
        assert_eq!(normalize_line_endings("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize_line_endings("a\nb\n"), "a\nb\n");
    }

    #[test]
    fn detect_line_ending_chooses_crlf_when_present() {
        assert_eq!(detect_line_ending("a\r\nb"), LineEnding::Crlf);
        assert_eq!(detect_line_ending("a\nb"), LineEnding::Lf);
    }

    #[test]
    fn convert_to_line_ending_round_trip() {
        assert_eq!(convert_to_line_ending("a\nb", LineEnding::Crlf), "a\r\nb");
        assert_eq!(convert_to_line_ending("a\r\nb", LineEnding::Lf), "a\r\nb");
    }

    #[tokio::test]
    async fn plan_edit_and_execute_edit_agree_on_disproportionate_match() {
        // 用一行会触发 line_trim 的输入,验证 plan 和 execute 给出相同 match。
        let path = temp_path("agree.txt");
        let content = "alpha   \nbeta\ngamma   \n";
        fs::write(&path, content).await.unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "alpha".to_string(),
            new_string: "ALPHA".to_string(),
            replace_all: None,
        };

        let plan = plan_edit(&input).await.expect("plan should succeed");
        let prepared = PreparedEdit {
            input: input.clone(),
            preview: build_preview(&input, &plan.matches, &plan.diff),
            matches: plan.matches.clone(),
        };
        let report = execute_edit(&prepared)
            .await
            .expect("execute should succeed");

        assert_eq!(
            plan.matches, report.matches,
            "plan and execute matches diverge"
        );

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn execute_edit_normalizes_crlf_in_old_string() {
        // CRLF 文件 + CRLF old_string:plan 走 original_old 精确匹配。
        let path = temp_path("crlf.txt");
        fs::write(&path, "alpha\r\nbeta\r\ngamma\r\n")
            .await
            .unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "beta\r\n".to_string(),
            new_string: "BETA\r\n".to_string(),
            replace_all: None,
        };
        let prepared = EditTool.prepare(input).await.unwrap();
        assert_eq!(prepared.matches.len(), 1);
        let result = EditTool
            .execute_prepared(prepared, ToolExecutionContext::test("edit"))
            .await;
        assert!(!result.is_error, "{}", result.output);

        assert_eq!(
            fs::read_to_string(&path).await.unwrap(),
            "alpha\r\nBETA\r\ngamma\r\n"
        );
        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn metadata_prepared_matches_equals_matches_on_unmodified_file() {
        let path = temp_path("meta_eq.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").await.unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "beta".to_string(),
            new_string: "BETA".to_string(),
            replace_all: None,
        };
        let prepared = EditTool.prepare(input).await.unwrap();
        let result = EditTool
            .execute_prepared(prepared, ToolExecutionContext::test("edit"))
            .await;
        assert!(!result.is_error, "{}", result.output);

        let metadata = result.metadata.expect("metadata should be set");
        let prepared_matches = metadata
            .get("prepared_matches")
            .expect("prepared_matches in metadata");
        let matches = metadata.get("matches").expect("matches in metadata");
        assert_eq!(
            prepared_matches, matches,
            "on unmodified file, prepared_matches should equal matches"
        );

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn permission_preview_diff_equals_execute_diff_on_unmodified_file() {
        let path = temp_path("diff_eq.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").await.unwrap();

        let input = EditInput {
            file_path: path.clone(),
            old_string: "beta".to_string(),
            new_string: "BETA".to_string(),
            replace_all: None,
        };
        let prepared = EditTool.prepare(input).await.unwrap();
        let preview_diff = prepared.preview.diff.clone();
        assert!(
            !preview_diff.is_empty(),
            "preview diff should be non-empty for a successful prepare"
        );

        let result = EditTool
            .execute_prepared(prepared, ToolExecutionContext::test("edit"))
            .await;
        assert!(!result.is_error, "{}", result.output);

        let metadata = result.metadata.expect("metadata should be set");
        let execute_diff = metadata
            .get("diff")
            .and_then(|v| v.as_str())
            .expect("diff in metadata");
        assert_eq!(
            preview_diff, execute_diff,
            "permission preview diff must equal execute diff"
        );

        let _ = fs::remove_file(path).await;
    }
}
