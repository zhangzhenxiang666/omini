use super::{Tool, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::Path;
use tokio::fs;

// ===========================================================================
// 输入类型
// ===========================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyPatchInput {
    /// The entire contents of the apply_patch command
    pub input: String,
}

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    type Input = ApplyPatchInput;

    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        concat!(
            "Edit files via a structured patch format. Patch is wrapped in *** Begin Patch / *** End Patch.\n",
            "\n",
            "Operations:\n",
            "  *** Add File: <path>     -- create new file (lines start with +, no anchor)\n",
            "  *** Delete File: <path>  -- delete existing file\n",
            "  *** Update File: <path>  -- modify file with @@ hunks\n",
            "    *** Move to: <path>    -- (optional) rename after applying hunks\n",
            "\n",
            "Each hunk starts with @@ L<N> (N = 1-indexed line number):\n",
            "  @@ L<N> is REQUIRED for every hunk\n",
            "  Multiple hunks per file allowed (each with its own @@ L<N>)\n",
            "  Offsets are computed automatically across hunks\n",
            "\n",
            "Line prefixes inside hunk:\n",
            "  ` ` (space)  Context -- must match EXACTLY (including whitespace and all characters), preserved in output\n",
            "  `-`          Remove -- must match EXACTLY (including whitespace and all characters)\n",
            "  `+`          Add\n",
            "\n",
            "Rules:\n",
            "  - Include 3 lines of context before and after each change.\n",
            "  - Append *** End of File after a hunk to add content at file end.\n",
            "  - All paths MUST be absolute.\n",
            "  - Context and removal lines must match exactly (use exact content from `read`, including whitespace).\n",
            "  - On failure, error shows the closest match.\n",
            "\n",
            "Example (add + update + delete):\n",
            "\n",
            "*** Begin Patch\n",
            "*** Add File: /project/src/hello.rs\n",
            "+pub fn greet() {\n",
            "+    println!(\"hi\");\n",
            "+}\n",
            "*** Update File: /project/src/main.rs\n",
            "@@ L42\n",
            " fn main() {\n",
            "-    old_thing();\n",
            "+    greet();\n",
            " }\n",
            "*** Delete File: /project/src/old.rs\n",
            "*** End Patch"
        )
    }

    async fn execute(&self, input: ApplyPatchInput) -> ToolResult {
        match apply_patch(&input.input).await {
            Ok(report) => ToolResult::ok(report),
            Err(e) => ToolResult::error(e),
        }
    }
}

// ===========================================================================
// 解析后的表示
// ===========================================================================

#[derive(Debug)]
pub enum PatchOp {
    Add {
        path: String,
        content: Vec<String>,
    },
    Update {
        path: String,
        new_path: Option<String>,
        hunks: Vec<Hunk>,
    },
    Delete {
        path: String,
    },
}

#[derive(Debug)]
pub struct Hunk {
    /// 行号锚点，从 @@ L<N> 中提取（从 1 开始）
    pub line_anchor: usize,
    /// 要在文件中搜索的行（上下文 + 删除行，不含前缀）
    pub before: Vec<String>,
    /// 替换后的行（上下文 + 新增行，不含前缀）
    pub after: Vec<String>,
    /// 如果为 true，则在文件末尾匹配
    pub is_end_of_file: bool,
}

// ===========================================================================
// 解析器
// ===========================================================================

#[derive(Debug)]
struct ParseCtx {
    lines: Vec<String>,
    pos: usize,
}

impl ParseCtx {
    fn new(input: &str) -> Self {
        Self {
            lines: input.lines().map(|l| l.to_string()).collect(),
            pos: 0,
        }
    }

    fn cur(&self) -> Option<&str> {
        self.lines.get(self.pos).map(|s| s.as_str())
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn skip_empty_lines(&mut self) {
        while self.pos < self.lines.len() && self.lines[self.pos].trim().is_empty() {
            self.pos += 1;
        }
    }

    fn lineno(&self) -> usize {
        self.pos + 1
    }

    fn error(&self, msg: String) -> String {
        format!("Line {}: {}", self.lineno(), msg)
    }
}

pub fn parse_patch(input: &str) -> Result<Vec<PatchOp>, String> {
    let mut ctx = ParseCtx::new(input);
    let mut ops: Vec<PatchOp> = Vec::new();

    // 期望: "*** Begin Patch"
    ctx.skip_empty_lines();
    match ctx.cur() {
        Some(l) if l.trim() == "*** Begin Patch" => ctx.advance(),
        Some(l) => return Err(ctx.error(format!("Expected '*** Begin Patch', got: {l}"))),
        None => return Err("Empty input, expected '*** Begin Patch'".to_string()),
    }

    loop {
        ctx.skip_empty_lines();

        match ctx.cur() {
            None => return Err("Missing '*** End Patch' terminator".to_string()),
            Some(l) if l.trim() == "*** End Patch" => {
                ctx.advance();
                return Ok(ops);
            }
            Some(l) if l.trim().starts_with("*** Add File: ") => {
                let path = l
                    .trim()
                    .strip_prefix("*** Add File: ")
                    .unwrap()
                    .trim()
                    .to_string();
                ctx.advance();
                let content = parse_add_content(&mut ctx)?;
                ops.push(PatchOp::Add { path, content });
            }
            Some(l) if l.trim().starts_with("*** Update File: ") => {
                let path = l
                    .trim()
                    .strip_prefix("*** Update File: ")
                    .unwrap()
                    .trim()
                    .to_string();
                ctx.advance();
                let (hunks, new_path) = parse_update_hunks(&mut ctx)?;
                ops.push(PatchOp::Update {
                    path,
                    new_path,
                    hunks,
                });
            }
            Some(l) if l.trim().starts_with("*** Delete File: ") => {
                let path = l
                    .trim()
                    .strip_prefix("*** Delete File: ")
                    .unwrap()
                    .trim()
                    .to_string();
                ctx.advance();
                ops.push(PatchOp::Delete { path });
            }
            Some(l) => {
                return Err(ctx.error(format!(
                    "Expected operation header (*** Add/Update/Delete File: ...), got: {l}"
                )));
            }
        }
    }
}

/// 解析 `*** Add File:` 后的内容行。收集所有以 `+` 开头的行。
fn parse_add_content(ctx: &mut ParseCtx) -> Result<Vec<String>, String> {
    let mut content = Vec::new();
    loop {
        ctx.skip_empty_lines();
        match ctx.cur() {
            Some(l) if l.starts_with('+') => {
                content.push(l.strip_prefix('+').unwrap().to_string());
                ctx.advance();
            }
            Some(l) if l.starts_with("*** ") => {
                return Ok(content);
            }
            Some(l) => {
                return Err(ctx.error(format!(
                    "In Add File block: expected '+' line or next operation, got: {l}"
                )));
            }
            None => {
                return Ok(content);
            }
        }
    }
}

/// 解析 `*** Update File:` 后的 hunk。返回（hunks, 可选的重命名目标）。
fn parse_update_hunks(ctx: &mut ParseCtx) -> Result<(Vec<Hunk>, Option<String>), String> {
    let mut hunks = Vec::new();
    let mut new_path: Option<String> = None;

    loop {
        ctx.skip_empty_lines();

        match ctx.cur() {
            // 重命名指令
            Some(l) if l.trim().starts_with("*** Move to: ") => {
                let target = l
                    .trim()
                    .strip_prefix("*** Move to: ")
                    .unwrap()
                    .trim()
                    .to_string();
                if new_path.is_some() {
                    return Err(ctx.error("Duplicate '*** Move to:' directive".to_string()));
                }
                new_path = Some(target);
                ctx.advance();
            }
            // 新 hunk
            Some(l) if l.trim().starts_with("@@") => {
                let hunk = parse_hunk(ctx)?;
                hunks.push(hunk);
            }
            // 下一个操作或结束
            Some(l) if l.trim().starts_with("*** ") => {
                return Ok((hunks, new_path));
            }
            Some(l) => {
                return Err(ctx.error(format!(
                    "In Update File block: expected hunk ('@@'), directive, or next operation, got: {l}"
                )));
            }
            None => {
                return Ok((hunks, new_path));
            }
        }
    }
}

/// 解析一个以 `@@` 开头的 hunk。
fn parse_hunk(ctx: &mut ParseCtx) -> Result<Hunk, String> {
    let mut line_anchor: Option<usize> = None;
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut is_end_of_file = false;

    // 收集 @@ 头部行
    while let Some(l) = ctx.cur() {
        let trimmed = l.trim();
        if trimmed.starts_with("@@") {
            // @@ 行必须指定行号锚点 L<N>
            let n = extract_line_number(trimmed).ok_or_else(|| {
                format!(
                    "Line {}: @@ must specify line number (e.g. @@ L42)",
                    ctx.lineno()
                )
            })?;
            if line_anchor.is_none() {
                line_anchor = Some(n);
            } else {
                return Err(format!(
                    "Line {}: Multiple @@ lines not allowed (only one @@ L<N> per hunk)",
                    ctx.lineno()
                ));
            }
            ctx.advance();
        } else {
            break;
        }
    }

    // 收集 hunk 主体行
    loop {
        match ctx.cur() {
            Some(l) if l.trim() == "*** End of File" => {
                is_end_of_file = true;
                ctx.advance();
            }
            Some(l) if l.starts_with(' ') => {
                let content = l.strip_prefix(' ').unwrap().to_string();
                before.push(content.clone());
                after.push(content);
                ctx.advance();
            }
            Some(l) if l.starts_with('-') => {
                before.push(l.strip_prefix('-').unwrap().to_string());
                ctx.advance();
            }
            Some(l) if l.starts_with('+') => {
                after.push(l.strip_prefix('+').unwrap().to_string());
                ctx.advance();
            }
            _ => {
                break;
            }
        }
    }

    if before.is_empty() && after.is_empty() {
        return Err(format!(
            "Line {}: Empty hunk (no content lines)",
            ctx.lineno()
        ));
    }

    let line_anchor = line_anchor.ok_or_else(|| {
        format!(
            "Line {}: @@ must specify line number (e.g. @@ L42)",
            ctx.lineno()
        )
    })?;

    Ok(Hunk {
        before,
        line_anchor,
        after,
        is_end_of_file,
    })
}

/// 从 @@ 行中提取从 1 开始的行号，例如 "@@ L42" -> Some(42)。
fn extract_line_number(s: &str) -> Option<usize> {
    let content = s.trim().strip_prefix("@@")?.trim();
    // 找到第一个 'L' 后跟数字
    let l_pos = content.find('L')?;
    let after_l = &content[l_pos + 1..];
    let num_str: String = after_l.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        None
    } else {
        num_str.parse().ok()
    }
}

// ===========================================================================
// 上下文匹配器
// ===========================================================================

#[derive(Debug)]
enum MatchError {
    NotFound { hint: Option<(usize, String)> },
    Ambiguous { positions: Vec<usize> },
}

/// 在文件行中查找 hunk 的 before 上下文匹配的位置。
/// 返回从 0 开始的起始位置。
fn find_hunk_position(file_lines: &[String], hunk: &Hunk) -> Result<usize, MatchError> {
    // 特殊情况：End of File 标记表示追加到末尾
    if hunk.is_end_of_file {
        return Ok(file_lines.len());
    }

    let pattern = &hunk.before;
    if pattern.is_empty() {
        return Ok(hunk.line_anchor.saturating_sub(1));
    }

    if pattern.len() > file_lines.len() {
        return Err(MatchError::NotFound { hint: None });
    }

    let anchor = hunk.line_anchor;
    // 限制搜索范围在锚点 ±3 行内
    let start = anchor.saturating_sub(4);
    let end = std::cmp::min(file_lines.len(), anchor + 3);
    if start >= end || pattern.len() > end - start {
        return Err(MatchError::NotFound { hint: None });
    }

    let search_window = &file_lines[start..end];

    // 在搜索窗口内查找所有精确匹配
    let matches: Vec<usize> = search_window
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, window)| {
            window
                .iter()
                .map(|s| s.as_str())
                .eq(pattern.iter().map(|s| s.as_str()))
        })
        .map(|(i, _)| start + i)
        .collect();

    match matches.len() {
        0 => {
            let hint = find_closest_match(search_window, pattern);
            let hint = hint.map(|(pos, desc)| (start + pos, desc));
            Err(MatchError::NotFound { hint })
        }
        1 => Ok(matches[0]),
        _ => Err(MatchError::Ambiguous {
            positions: matches.iter().map(|p| p + 1).collect(),
        }),
    }
}

/// 查找最接近的模糊匹配，用于生成有帮助的错误信息。
fn find_closest_match(file_lines: &[String], pattern: &[String]) -> Option<(usize, String)> {
    // 尝试部分前缀匹配
    for len in (1..=pattern.len()).rev() {
        let sub: Vec<&str> = pattern[..len].iter().map(|s| s.as_str()).collect();
        for (i, window) in file_lines.windows(len).enumerate() {
            if window.iter().map(|s| s.as_str()).eq(sub.iter().copied()) {
                let desc = if len == pattern.len() {
                    "exact match but at unexpected position".to_string()
                } else {
                    format!("partial match: matched first {len} line(s), rest differs")
                };
                return Some((i + 1, desc));
            }
        }
    }

    // 检查首行的空白差异
    if let Some(first) = pattern.first() {
        if let Some((i, _)) = file_lines.iter().enumerate().find(|(_, l)| *l == first) {
            return Some((i + 1, "first line matched but rest differs".to_string()));
        }
        // 忽略空白差异
        let first_normalized: String = first.chars().filter(|c| !c.is_whitespace()).collect();
        for (i, line) in file_lines.iter().enumerate() {
            let line_normalized: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if line_normalized == first_normalized {
                return Some((
                    i + 1,
                    "first line matches when ignoring whitespace (indentation differs)".to_string(),
                ));
            }
        }
    }

    None
}

// ===========================================================================
// 执行器
// ===========================================================================

/// 顺序执行所有解析后的操作。
/// 失败时返回包含详细执行报告的错误信息。
async fn apply_ops(ops: &[PatchOp]) -> Result<String, String> {
    let total = ops.len();
    let mut completed: Vec<String> = Vec::new();

    for (i, op) in ops.iter().enumerate() {
        let step = i + 1;
        match apply_single_op(op).await {
            Ok(msg) => {
                completed.push(format!("  [{step}/{total}] OK -- {msg}"));
            }
            Err(e) => {
                let mut report = format!("apply_patch: Operation {step}/{total} failed\n\n");
                report.push_str("Completed operations:\n");
                for c in &completed {
                    report.push_str(c);
                    report.push('\n');
                }
                report.push('\n');
                report.push_str(&format!("  [{step}/{total}] FAIL -- {e}\n"));
                for remaining in (step + 1)..=total {
                    report.push_str(&format!(
                        "  [{remaining}/{total}] SKIP -- (depends on previous)\n"
                    ));
                }
                return Err(report);
            }
        }
    }

    let mut report = format!("apply_patch: All {total} operation(s) completed successfully\n\n");
    for c in &completed {
        report.push_str(c);
        report.push('\n');
    }
    Ok(report.trim().to_string())
}

/// 应用单个 patch 操作。
async fn apply_single_op(op: &PatchOp) -> Result<String, String> {
    match op {
        PatchOp::Add { path, content } => apply_add(path, content).await,
        PatchOp::Update {
            path,
            new_path,
            hunks,
        } => apply_update(path, new_path.as_deref(), hunks).await,
        PatchOp::Delete { path } => apply_delete(path).await,
    }
}

/// 添加新文件。
async fn apply_add(path: &str, content: &[String]) -> Result<String, String> {
    let p = Path::new(path);
    if p.exists() {
        return Err(format!("File already exists: {path}"));
    }

    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create parent directories for {path}: {e}"))?;
    }

    let data = content.join("\n");
    fs::write(p, &data)
        .await
        .map_err(|e| format!("Failed to write {path}: {e}"))?;

    Ok(format!("Created {path} ({} lines)", content.len()))
}

/// 用 hunks 更新已有文件。
async fn apply_update(
    path: &str,
    new_path: Option<&str>,
    hunks: &[Hunk],
) -> Result<String, String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("File not found: {path}"));
    }

    let content = fs::read_to_string(p)
        .await
        .map_err(|e| format!("Failed to read {path}: {e}"))?;

    let mut file_lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    let mut total_matched = 0usize;
    let mut total_removed = 0usize;
    let mut total_added = 0usize;
    let mut offset: isize = 0;

    for (hunk_idx, hunk) in hunks.iter().enumerate() {
        // 根据之前 hunk 累积的偏移量调整锚点
        let adjusted_anchor = {
            let adj = (hunk.line_anchor as isize + offset) as usize;
            if adj < 1 { 1 } else { adj }
        };
        let adjusted_hunk = Hunk {
            line_anchor: adjusted_anchor,
            before: hunk.before.clone(),
            is_end_of_file: hunk.is_end_of_file,
            after: hunk.after.clone(),
        };
        let pos = find_hunk_position(&file_lines, &adjusted_hunk).map_err(|e| match e {
            MatchError::NotFound { hint } => {
                let hint_msg = match hint {
                    Some((ln, desc)) => format!("  Closest match at line {ln}: {desc}"),
                    None => "  No similar content found in file.".to_string(),
                };
                format!(
                    "Hunk {} (L{}) for {path}: Could not find matching context\n{hint_msg}",
                    hunk_idx + 1,
                    hunk.line_anchor,
                )
            }
            MatchError::Ambiguous { positions } => {
                let pos_str: Vec<String> = positions.iter().map(|p| format!("line {p}")).collect();
                format!(
                    "Hunk {} (L{}) for {path}: Context matches at multiple locations:\n  {}\n \
                    Use a more specific line number or add more context lines.",
                    hunk_idx + 1,
                    hunk.line_anchor,
                    pos_str.join(", "),
                )
            }
        })?;

        // 统计变更
        let ctx_lines: std::collections::HashSet<&str> = hunk
            .before
            .iter()
            .filter(|l| hunk.after.contains(l))
            .map(|s| s.as_str())
            .collect();

        let removals = hunk.before.len() - ctx_lines.len();
        let additions = hunk.after.len() - ctx_lines.len();
        total_removed += removals;
        total_added += additions;
        total_matched += 1;

        // 应用 hunk
        apply_hunk(&mut file_lines, pos, hunk);

        // 更新偏移量：净变更行数
        offset += hunk.after.len() as isize - hunk.before.len() as isize;
    }

    // 写入更新后的内容
    let new_content = file_lines.join("\n");
    fs::write(p, &new_content)
        .await
        .map_err(|e| format!("Failed to write {path}: {e}"))?;

    // 处理重命名
    if let Some(target) = new_path {
        let target_p = Path::new(target);
        if let Some(parent) = target_p.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent directories for {target}: {e}"))?;
        }
        fs::rename(p, target_p)
            .await
            .map_err(|e| format!("Failed to rename {path} to {target}: {e}"))?;

        Ok(format!(
            "Updated and renamed {path} -> {target} ({} hunks, -{}/+{} lines)",
            total_matched, total_removed, total_added,
        ))
    } else {
        Ok(format!(
            "Updated {path} ({} hunks, -{}/+{} lines)",
            total_matched, total_removed, total_added,
        ))
    }
}

/// 在指定位置将单个 hunk 应用到文件行。
fn apply_hunk(file_lines: &mut Vec<String>, pos: usize, hunk: &Hunk) {
    if hunk.is_end_of_file {
        for line in &hunk.after {
            file_lines.push(line.clone());
        }
        return;
    }

    let pattern_len = hunk.before.len();
    file_lines.drain(pos..pos + pattern_len);

    let mut insert_pos = pos;
    for line in &hunk.after {
        file_lines.insert(insert_pos, line.clone());
        insert_pos += 1;
    }
}

/// 删除文件。
async fn apply_delete(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("File not found: {path}"));
    }

    fs::remove_file(p)
        .await
        .map_err(|e| format!("Failed to delete {path}: {e}"))?;

    Ok(format!("Deleted {path}"))
}

// ===========================================================================
// 顶层入口
// ===========================================================================

async fn apply_patch(input: &str) -> Result<String, String> {
    let ops = parse_patch(input)?;

    if ops.is_empty() {
        return Err("No operations found in patch".to_string());
    }

    apply_ops(&ops).await
}

// ===========================================================================
// 测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 解析器测试 ----

    #[test]
    fn test_parse_add_file() {
        let input = concat!(
            "*** Begin Patch\n",
            "*** Add File: /tmp/test.txt\n",
            "+Hello, world!\n",
            "+Second line\n",
            "*** End Patch",
        );
        let ops = parse_patch(input).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOp::Add { path, content } => {
                assert_eq!(path, "/tmp/test.txt");
                assert_eq!(
                    content,
                    &vec!["Hello, world!".to_string(), "Second line".to_string()]
                );
            }
            _ => panic!("Expected Add op"),
        }
    }

    #[test]
    fn test_parse_delete_file() {
        let input = concat!(
            "*** Begin Patch\n",
            "*** Delete File: /tmp/old.txt\n",
            "*** End Patch",
        );
        let ops = parse_patch(input).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOp::Delete { path } => assert_eq!(path, "/tmp/old.txt"),
            _ => panic!("Expected Delete op"),
        }
    }

    #[test]
    fn test_parse_update_file() {
        let input = concat!(
            "*** Begin Patch\n",
            "*** Update File: /tmp/test.txt\n",
            "@@ L10\n",
            " line1\n",
            "-old_line\n",
            "+new_line\n",
            " line3\n",
            "*** End Patch",
        );
        let ops = parse_patch(input).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOp::Update {
                path,
                new_path,
                hunks,
            } => {
                assert_eq!(path, "/tmp/test.txt");
                assert!(new_path.is_none());
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].line_anchor, 10);
                assert_eq!(hunks[0].before, vec!["line1", "old_line", "line3"]);
                assert_eq!(hunks[0].after, vec!["line1", "new_line", "line3"]);
            }
            _ => panic!("Expected Update op"),
        }
    }

    #[test]
    fn test_parse_update_with_move() {
        let input = concat!(
            "*** Begin Patch\n",
            "*** Update File: /tmp/old.rs\n",
            "*** Move to: /tmp/new.rs\n",
            "@@ L1\n",
            " fn main() {}\n",
            "*** End Patch",
        );
        let ops = parse_patch(input).unwrap();
        match &ops[0] {
            PatchOp::Update { path, new_path, .. } => {
                assert_eq!(path, "/tmp/old.rs");
                assert_eq!(new_path.as_deref(), Some("/tmp/new.rs"));
            }
            _ => panic!("Expected Update op"),
        }
    }

    #[test]
    fn test_parse_multi_ops() {
        let input = concat!(
            "*** Begin Patch\n",
            "*** Add File: /tmp/a.txt\n",
            "+content a\n",
            "*** Update File: /tmp/b.txt\n",
            "@@ L5\n",
            "-old\n",
            "+new\n",
            "*** Delete File: /tmp/c.txt\n",
            "*** End Patch",
        );
        let ops = parse_patch(input).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], PatchOp::Add { .. }));
        assert!(matches!(&ops[1], PatchOp::Update { .. }));
        assert!(matches!(&ops[2], PatchOp::Delete { .. }));
    }

    #[test]
    fn test_parse_empty_input() {
        assert!(parse_patch("").is_err());
    }

    #[test]
    fn test_parse_missing_begin() {
        assert!(parse_patch("*** End Patch").is_err());
    }

    #[test]
    fn test_parse_missing_end() {
        let input = concat!("*** Begin Patch\n", "*** Add File: /tmp/a.txt\n", "+hello");
        assert!(parse_patch(input).is_err());
    }

    #[test]
    fn test_parse_end_of_file_marker() {
        let input = concat!(
            "*** Begin Patch\n",
            "*** Update File: /tmp/test.txt\n",
            "@@ L2\n",
            " line1\n",
            "+new_line\n",
            "*** End of File\n",
            "*** End Patch",
        );
        let ops = parse_patch(input).unwrap();
        match &ops[0] {
            PatchOp::Update { hunks, .. } => {
                assert!(hunks[0].is_end_of_file);
                assert_eq!(hunks[0].line_anchor, 2);
                assert_eq!(hunks[0].before, vec!["line1"]);
                assert_eq!(hunks[0].after, vec!["line1", "new_line"]);
            }
            _ => panic!("Expected Update op"),
        }
    }

    #[test]
    fn test_parse_multi_header_lines() {
        let input = concat!(
            "*** Begin Patch\n",
            "*** Update File: /tmp/test.txt\n",
            "@@ L42\n",
            "-old_code\n",
            "+new_code\n",
            "*** End Patch",
        );
        let ops = parse_patch(input).unwrap();
        match &ops[0] {
            PatchOp::Update { hunks, .. } => {
                assert_eq!(hunks[0].line_anchor, 42);
            }
            _ => panic!("Expected Update op"),
        }
    }

    // ---- 上下文匹配器测试 ----

    fn make_lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_match_simple() {
        let file = make_lines(&["a", "b", "c", "d", "e"]);
        let hunk = Hunk {
            line_anchor: 2,
            before: make_lines(&["b", "c"]),
            after: make_lines(&["b", "x"]),
            is_end_of_file: false,
        };
        assert_eq!(find_hunk_position(&file, &hunk).unwrap(), 1);
    }

    #[test]
    fn test_match_not_found() {
        let file = make_lines(&["a", "b", "c"]);
        let hunk = Hunk {
            line_anchor: 2,
            before: make_lines(&["x", "y"]),
            after: make_lines(&["x", "z"]),
            is_end_of_file: false,
        };
        assert!(find_hunk_position(&file, &hunk).is_err());
    }

    #[test]
    fn test_match_ambiguous() {
        let file = make_lines(&["a", "b", "c", "a", "b", "c", "d"]);
        let hunk = Hunk {
            line_anchor: 4,
            before: make_lines(&["a", "b", "c"]),
            after: make_lines(&["a", "x", "c"]),
            is_end_of_file: false,
        };
        let result = find_hunk_position(&file, &hunk);
        assert!(result.is_err());
        if let Err(MatchError::Ambiguous { positions }) = result {
            assert_eq!(positions.len(), 2);
        } else {
            panic!("Expected Ambiguous error");
        }
    }

    #[test]
    fn test_match_end_of_file() {
        let file = make_lines(&["a", "b", "c"]);
        let hunk = Hunk {
            line_anchor: 1,
            before: make_lines(&[]),
            after: make_lines(&["d"]),
            is_end_of_file: true,
        };
        assert_eq!(find_hunk_position(&file, &hunk).unwrap(), 3);
    }

    // ---- Hunk 应用测试 ----
    #[test]
    fn test_apply_hunk_replace() {
        let mut lines = make_lines(&["a", "b", "c", "d"]);
        let hunk = Hunk {
            line_anchor: 2,
            before: make_lines(&["b", "c"]),
            after: make_lines(&["b", "x"]),
            is_end_of_file: false,
        };
        apply_hunk(&mut lines, 1, &hunk);
        assert_eq!(lines, make_lines(&["a", "b", "x", "d"]));
    }

    #[test]
    fn test_apply_hunk_append_at_end() {
        let mut lines = make_lines(&["a", "b"]);
        let hunk = Hunk {
            line_anchor: 1,
            before: make_lines(&[]),
            after: make_lines(&["c"]),
            is_end_of_file: true,
        };
        apply_hunk(&mut lines, 2, &hunk);
        assert_eq!(lines, make_lines(&["a", "b", "c"]));
    }

    #[test]
    fn test_apply_hunk_insert() {
        let mut lines = make_lines(&["a", "d"]);
        let hunk = Hunk {
            line_anchor: 1,
            before: make_lines(&["a"]),
            after: make_lines(&["a", "b", "c"]),
            is_end_of_file: false,
        };
        apply_hunk(&mut lines, 0, &hunk);
        assert_eq!(lines, make_lines(&["a", "b", "c", "d"]));
    }

    #[test]
    fn test_apply_hunk_delete() {
        let mut lines = make_lines(&["a", "b", "c"]);
        let hunk = Hunk {
            line_anchor: 2,
            before: make_lines(&["b"]),
            after: make_lines(&[]),
            is_end_of_file: false,
        };
        apply_hunk(&mut lines, 1, &hunk);
        assert_eq!(lines, make_lines(&["a", "c"]));
    }

    // ---- 集成测试 ----

    #[tokio::test]
    async fn test_apply_patch_add_file() {
        let dir =
            std::env::temp_dir().join(format!("omini_test_{}_{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let file_path = dir.join("new_file.txt");
        let patch = {
            let p = file_path.display().to_string();
            format!(
                "*** Begin Patch\n*** Add File: {p}\n+Hello, world!\n+Second line\n*** End Patch"
            )
        };

        let result = apply_patch(&patch).await.unwrap();
        assert!(result.contains("Created"));
        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Hello, world!\nSecond line");

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_apply_patch_update_file() {
        let dir =
            std::env::temp_dir().join(format!("omini_test_{}_{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let file_path = dir.join("test.txt");
        fs::write(&file_path, "line1\nold_line\nline3\n")
            .await
            .unwrap();

        let patch = {
            let p = file_path.display().to_string();
            format!(
                "*** Begin Patch\n*** Update File: {p}\n@@ L1\n line1\n-old_line\n+new_line\n line3\n*** End Patch"
            )
        };

        let result = apply_patch(&patch).await.unwrap();
        assert!(result.contains("Updated"));
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "line1\nnew_line\nline3");

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_apply_patch_delete_file() {
        let dir =
            std::env::temp_dir().join(format!("omini_test_{}_{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let file_path = dir.join("to_delete.txt");
        fs::write(&file_path, "content").await.unwrap();

        let patch = {
            let p = file_path.display().to_string();
            format!("*** Begin Patch\n*** Delete File: {p}\n*** End Patch")
        };

        let result = apply_patch(&patch).await.unwrap();
        assert!(result.contains("Deleted"));
        assert!(!file_path.exists());

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_apply_patch_update_nonexistent_fails() {
        let patch = concat!(
            "*** Begin Patch\n",
            "*** Update File: /tmp/__nonexistent_file_xyz__/test.txt\n",
            "@@ L1\n",
            " old\n",
            "-bad\n",
            "+good\n",
            "*** End Patch",
        );
        let result = apply_patch(patch).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_apply_patch_add_existing_fails() {
        let dir =
            std::env::temp_dir().join(format!("omini_test_{}_{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let file_path = dir.join("exists.txt");
        fs::write(&file_path, "existing").await.unwrap();

        let patch = {
            let p = file_path.display().to_string();
            format!("*** Begin Patch\n*** Add File: {p}\n+new content\n*** End Patch")
        };

        let result = apply_patch(&patch).await;
        assert!(result.is_err());

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_apply_patch_composite_fails_midway() {
        let dir =
            std::env::temp_dir().join(format!("omini_test_{}_{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let file1 = dir.join("keep.txt");
        let file2 = dir.join("modify.txt");
        fs::write(&file2, "old\ncontent\n").await.unwrap();

        // 三个操作：添加（应成功）、更新（应成功）、
        // 删除不存在的文件（应失败）
        let patch = {
            let p1 = file1.display().to_string();
            let p2 = file2.display().to_string();
            let p3 = dir.join("ghost.txt").display().to_string();
            format!(
                "*** Begin Patch\n*** Add File: {p1}\n+new file\n*** Update File: {p2}\n@@ L1\n-old\n+new\n content\n*** Delete File: {p3}\n*** End Patch"
            )
        };

        let result = apply_patch(&patch).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        // 应报告在第 3 步失败
        assert!(err.contains("[3/3]") || err.contains("Failed"));
        // 应提及步骤 1 和 2 已完成
        assert!(err.contains("[1/3]") || err.contains("Completed"));
        // file1 应存在（步骤 1 成功）
        assert!(file1.exists());

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_apply_patch_empty_file_append() {
        let dir =
            std::env::temp_dir().join(format!("omini_test_{}_{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let file_path = dir.join("empty.txt");
        fs::write(&file_path, "").await.unwrap();

        let patch = {
            let p = file_path.display().to_string();
            format!(
                "*** Begin Patch\n*** Update File: {p}\n@@ L1\n+first line\n*** End of File\n*** End Patch"
            )
        };

        let result = apply_patch(&patch).await.unwrap();
        assert!(result.contains("Updated"));
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "first line");

        let _ = fs::remove_dir_all(&dir).await;
    }
}
