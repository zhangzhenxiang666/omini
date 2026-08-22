use super::{Tool, ToolExecutionContext, ToolResult, tool_metadata};
use async_trait::async_trait;
use omini_domain::events::{PermissionPreview, SearchPermissionPreview};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const DEFAULT_MAX_RESULTS: usize = 100;
const OUTPUT_BYTE_LIMIT: usize = 256 * 1024;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TEXT_LEN: usize = 2000;
const INVALID_PATTERN_MARKERS: &[&str] = &["regex parse error", "regex error", "unrecognized flag"];

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Content,
    Files,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchInput {
    /// Text or regex pattern to search for. In files mode, this filters file paths by substring.
    pub query: String,
    /// Search mode: content searches file contents, files searches file names. Defaults to content.
    #[serde(default)]
    pub mode: SearchMode,
    /// File or directory to search. Defaults to the current working directory.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional glob passed to rg --glob to filter files (e.g. "*.rs"). Prefix with ! to exclude.
    #[serde(default)]
    pub include: Option<String>,
}

pub struct SearchTool;

#[derive(Debug)]
pub struct PreparedSearch {
    mode: SearchMode,
    query: String,
    raw_path: Option<PathBuf>,
    include: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RgRecord {
    Match {
        data: RgMatchData,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RgMatchData {
    path: RgTextField,
    lines: RgTextField,
    line_number: u64,
}

#[derive(Debug, Deserialize)]
struct RgTextField {
    text: String,
}

struct ParsedMatch {
    path: String,
    line: u64,
    text: String,
}

struct ParsedSearch {
    matches: Vec<ParsedMatch>,
    files_with_matches: usize,
    truncated: bool,
}

#[async_trait]
impl Tool for SearchTool {
    type Input = SearchInput;
    type Prepared = PreparedSearch;

    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        concat!(
            "Search local project files using ripgrep (`rg`). This is the primary tool for\n",
            "finding code, text, symbols, filenames, and file lists in the current project.\n",
            "\n",
            "Use this instead of `bash` commands like `rg`, `grep`, `find`, or `ls` for\n",
            "local code search and filename lookup.\n",
            "\n",
            "Examples:\n",
            "  {\"query\":\"ToolRegistry\"}\n",
            "  {\"query\":\"main.rs\",\"mode\":\"files\"}\n",
            "  {\"query\":\"struct .*Tool\",\"include\":\"*.rs\"}\n",
            "\n",
            "Input:\n",
            "  query   Text or regex pattern. Required for content mode; in files mode it filters file paths by substring.\n",
            "  mode    `content` to search file contents, `files` to search file paths. Defaults to content.\n",
            "  path    File or directory to search. Defaults to current working directory.\n",
            "  include Optional rg glob to filter files (e.g. \"*.rs\"). Prefix with ! to exclude.\n",
            "\n",
            "Rules:\n",
            "  - Search is read-only.\n",
            "  - Searches inside the current workspace or /tmp are allowed by default.\n",
            "  - Searches outside those locations, or inside private paths, require permission.\n",
            "  - The `.git` directory is always excluded.\n",
            "  - Use `read` after search when you need a larger code window.\n",
            "\n",
            "Output:\n",
            "  - Content matches are grouped by file as `<path>:\n  Line N: <text>`.\n",
            "  - First line is `Found N matches`; truncated output includes a hint."
        )
    }

    async fn prepare(&self, input: SearchInput) -> Result<Self::Prepared, ToolResult> {
        prepare_search(input).map_err(ToolResult::error)
    }

    fn permission_preview(&self, prepared: &Self::Prepared) -> Option<PermissionPreview> {
        // permission_preview 阶段没有 ctx，先用 raw_path 显示（可能是相对路径或 None）
        let path_display = prepared
            .raw_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".to_string());
        Some(PermissionPreview::Search(SearchPermissionPreview {
            query: prepared.query.clone(),
            mode: match prepared.mode {
                SearchMode::Content => "content",
                SearchMode::Files => "files",
            }
            .to_string(),
            path: path_display,
        }))
    }

    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        let thread_cwd = ctx.settings.cwd.clone();
        execute_search(prepared, thread_cwd).await
    }
}

fn prepare_search(input: SearchInput) -> Result<PreparedSearch, String> {
    if input.mode == SearchMode::Content && input.query.trim().is_empty() {
        return Err("query must not be empty in content mode".to_string());
    }

    // 路径解析推迟到 execute_prepared，因为那里有 ctx.settings.cwd（thread cwd），
    // 而 prepare 阶段只能用 std::env::current_dir()（守护进程下是 /tmp/omini，不对）。
    let raw_path = input.path.as_deref().and_then(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    });

    let include = input
        .include
        .map(|glob| glob.trim().to_string())
        .filter(|glob| !glob.is_empty());

    Ok(PreparedSearch {
        mode: input.mode,
        query: input.query,
        raw_path,
        include,
    })
}

async fn execute_search(prepared: PreparedSearch, thread_cwd: PathBuf) -> ToolResult {
    let rg_path = match bundled_rg_path() {
        Ok(path) => path,
        Err(error) => return ToolResult::error(error),
    };
    execute_search_with_rg_path(prepared, thread_cwd, &rg_path).await
}

async fn execute_search_with_rg_path(
    prepared: PreparedSearch,
    thread_cwd: PathBuf,
    rg_path: &Path,
) -> ToolResult {
    let output = match run_rg_with_path(&prepared, &thread_cwd, rg_path).await {
        Ok(output) => output,
        Err(e) => return ToolResult::error(e),
    };

    let path_display = thread_cwd.display().to_string();

    if !output.status.success() {
        if prepared.mode == SearchMode::Content && output.status.code() == Some(1) {
            let metadata = tool_metadata([
                ("mode", json!("content")),
                ("query", json!(prepared.query)),
                ("path", json!(path_display)),
                ("total", json!(0)),
                ("shown", json!(0)),
                ("truncated", json!(false)),
                ("files_with_matches", json!(0)),
            ]);
            return ToolResult::ok("(no matches)").with_metadata(metadata);
        }
        if prepared.mode == SearchMode::Content && output.status.code() == Some(2) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if INVALID_PATTERN_MARKERS
                .iter()
                .any(|marker| stderr.contains(marker))
            {
                let trimmed = stderr.trim();
                // rg 的错误信息本身以 "rg: " 开头,剥掉避免和我们的 "rg invalid pattern: " 前缀重复
                let cleaned = trimmed.strip_prefix("rg: ").unwrap_or(trimmed);
                return ToolResult::error(format!("rg invalid pattern: {cleaned}"));
            }
        }
        let exit_code = output.status.code().map_or("?".into(), |c| c.to_string());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let details = [stdout.trim(), stderr.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return ToolResult::error(format!("rg failed with exit code {exit_code}\n{details}"));
    }

    match prepared.mode {
        SearchMode::Content => {
            let parsed = match parse_rg_json(&output.stdout, DEFAULT_MAX_RESULTS) {
                Ok(parsed) => parsed,
                Err(e) => return ToolResult::error(format!("failed to parse rg output: {e}")),
            };
            let (text, shown) =
                format_grouped_results(&parsed, DEFAULT_MAX_RESULTS, OUTPUT_BYTE_LIMIT);
            let metadata = tool_metadata([
                ("mode", json!("content")),
                ("query", json!(prepared.query)),
                ("path", json!(path_display)),
                ("total", json!(parsed.matches.len())),
                ("shown", json!(shown)),
                ("truncated", json!(parsed.truncated)),
                ("files_with_matches", json!(parsed.files_with_matches)),
            ]);
            ToolResult::ok(text).with_metadata(metadata)
        }
        SearchMode::Files => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let lines = filter_file_lines(&stdout, &prepared.query);
            let total = lines.len();
            let (text, shown) = format_results(lines);
            let metadata = tool_metadata([
                ("mode", json!("files")),
                ("query", json!(prepared.query)),
                ("path", json!(path_display)),
                ("total", json!(total)),
                ("shown", json!(shown)),
                ("truncated", json!(shown < total)),
            ]);
            ToolResult::ok(text).with_metadata(metadata)
        }
    }
}

async fn run_rg_with_path(
    prepared: &PreparedSearch,
    thread_cwd: &Path,
    rg_path: &Path,
) -> Result<std::process::Output, String> {
    let mut command = Command::new(rg_path);
    // 无条件将 rg 的 cwd 设置为 thread cwd。
    // 守护进程下 process cwd 是 /tmp/omini，不能用。
    command.current_dir(thread_cwd);
    command.kill_on_drop(true);

    // 搜索目标：raw_path 为 None 时搜索 "."（即 thread cwd），否则使用用户指定的路径。
    // rg 会自动处理相对路径（相对于 cwd）和绝对路径。
    let search_target: OsString = prepared
        .raw_path
        .as_ref()
        .map(|p| p.as_os_str().to_os_string())
        .unwrap_or_else(|| OsString::from("."));

    match prepared.mode {
        SearchMode::Content => {
            command.args([
                "--no-config",
                "--json",
                "--hidden",
                "--no-messages",
                // 无条件排除 .git:!.git 屏蔽目录条目,!.git/** 屏蔽其下所有文件
                "--glob",
                "!.git",
                "--glob",
                "!.git/**",
            ]);
            if let Some(glob) = &prepared.include {
                command.arg("--glob").arg(glob);
            }
            command.arg("--");
            command.arg(&prepared.query);
            command.arg(&search_target);
        }
        SearchMode::Files => {
            command.args([
                "--files", "--color", "never", "--glob", "!.git", "--glob", "!.git/**",
            ]);
            if let Some(glob) = &prepared.include {
                command.arg("--glob").arg(glob);
            }
            command.arg(&search_target);
        }
    }

    match tokio::time::timeout(SEARCH_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "bundled ripgrep not found at {}; reinstall omini or restore bundled tools",
            bundled_rg_display_path()
        )),
        Ok(Err(e)) => Err(format!(
            "Failed to spawn bundled ripgrep at {}: {e}",
            rg_path.display()
        )),
        Err(_) => Err(format!(
            "search timed out after {}ms",
            SEARCH_TIMEOUT.as_millis()
        )),
    }
}

fn bundled_rg_path() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".omini").join("bin").join(rg_binary_name()))
        .ok_or_else(|| "cannot find home dir for bundled ripgrep".to_string())
}

fn bundled_rg_display_path() -> String {
    format!("~/.omini/bin/{}", rg_binary_name())
}

fn rg_binary_name() -> &'static str {
    if cfg!(windows) { "rg.exe" } else { "rg" }
}

fn filter_file_lines(stdout: &str, query: &str) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() {
        return stdout.lines().map(ToOwned::to_owned).collect();
    }
    stdout
        .lines()
        .filter(|line| line.contains(query))
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_rg_json(stdout: &[u8], cap: usize) -> Result<ParsedSearch, String> {
    let mut matches: Vec<ParsedMatch> = Vec::new();
    let mut files: BTreeSet<String> = BTreeSet::new();

    let iter = serde_json::Deserializer::from_slice(stdout).into_iter::<RgRecord>();
    for record in iter {
        let record = record.map_err(|e| e.to_string())?;
        let RgRecord::Match { data } = record else {
            continue;
        };
        files.insert(data.path.text.clone());
        matches.push(ParsedMatch {
            path: data.path.text,
            line: data.line_number,
            text: truncate_match_text(&data.lines.text),
        });
    }

    Ok(ParsedSearch {
        files_with_matches: files.len(),
        truncated: matches.len() > cap,
        matches,
    })
}

fn truncate_match_text(raw: &str) -> String {
    // rg --json 中 match 记录的 `lines.text` 末尾带换行符,这里剥掉
    let stripped = raw.strip_suffix('\n').unwrap_or(raw);
    if stripped.chars().count() > MAX_TEXT_LEN {
        let truncated: String = stripped.chars().take(MAX_TEXT_LEN).collect();
        format!("{truncated}...")
    } else {
        stripped.to_string()
    }
}

fn format_grouped_results(parsed: &ParsedSearch, cap: usize, byte_limit: usize) -> (String, usize) {
    if parsed.matches.is_empty() {
        return ("(no matches)".to_string(), 0);
    }

    let total = parsed.matches.len();
    let mut output = String::new();

    let mut header = format!("Found {} matches", total);
    if parsed.truncated {
        header.push_str(" (more matches available)");
    }
    header.push_str("\n\n");

    if header.len() > byte_limit {
        output.push_str(&header);
        return (output, 0);
    }
    output.push_str(&header);

    // 按 path 字典序稳定分组(BTreeMap 保证 lex order,Vec 保留原 match 顺序)
    let mut by_path: std::collections::BTreeMap<String, Vec<&ParsedMatch>> =
        std::collections::BTreeMap::new();
    for m in &parsed.matches {
        by_path.entry(m.path.clone()).or_default().push(m);
    }

    let mut shown = 0usize;
    let mut first_file = true;

    'outer: for (path, matches) in &by_path {
        if shown >= cap {
            break 'outer;
        }
        if !first_file {
            if output.len() + 1 > byte_limit {
                break 'outer;
            }
            output.push('\n');
        }
        first_file = false;

        let file_header = format!("{path}:\n");
        if output.len() + file_header.len() > byte_limit {
            break 'outer;
        }
        output.push_str(&file_header);

        for m in matches {
            if shown >= cap {
                break 'outer;
            }
            let line_str = format!("  Line {}: {}\n", m.line, m.text);
            if output.len() + line_str.len() > byte_limit {
                break 'outer;
            }
            output.push_str(&line_str);
            shown += 1;
        }
    }

    if shown < total {
        let hint = format!("(... output truncated; showing {shown} of {total} result lines ...)\n");
        output.push_str(&hint);
    }

    (output, shown)
}

fn format_results(lines: Vec<String>) -> (String, usize) {
    if lines.is_empty() {
        return ("(no matches)".to_string(), 0);
    }

    let total = lines.len();
    let mut output = String::new();
    let mut shown = 0usize;

    for line in lines {
        let next_len = line.len() + 1;
        if output.len() + next_len > OUTPUT_BYTE_LIMIT {
            break;
        }
        output.push_str(&line);
        output.push('\n');
        shown += 1;
    }

    if shown < total {
        output.push_str(&format!(
            "(... output truncated; showing {shown} of {total} result lines ...)\n"
        ));
    }

    (output.trim_end().to_string(), shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("omini-search-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create temp search dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, content).expect("write test file");
    }

    fn input(query: &str, path: &Path) -> SearchInput {
        SearchInput {
            query: query.to_string(),
            mode: SearchMode::Content,
            path: Some(path.display().to_string()),
            include: None,
        }
    }

    fn test_rg_path() -> PathBuf {
        let paths = std::env::var_os("PATH").expect("test environment should define PATH");
        std::env::split_paths(&paths)
            .map(|directory| directory.join(rg_binary_name()))
            .find(|candidate| {
                std::process::Command::new(candidate)
                    .arg("--version")
                    .output()
                    .is_ok()
            })
            .expect("test environment must provide rg on PATH")
    }

    async fn execute_for_test(prepared: PreparedSearch, cwd: &Path) -> ToolResult {
        execute_search_with_rg_path(prepared, cwd.to_path_buf(), &test_rg_path()).await
    }

    #[test]
    fn bundled_rg_display_path_uses_install_layout() {
        let expected = if cfg!(windows) {
            "~/.omini/bin/rg.exe"
        } else {
            "~/.omini/bin/rg"
        };

        assert_eq!(bundled_rg_display_path(), expected);
    }

    #[tokio::test]
    async fn content_search_finds_file_line_and_column() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/lib.rs"), "alpha beta\n");

        let prepared = SearchTool.prepare(input("beta", dir.path())).await.unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        assert!(result.output.contains("Found 1 matches"));
        assert!(result.output.contains("src/lib.rs:"));
        assert!(result.output.contains("  Line 1: alpha beta"));
        assert!(!result.output.contains("1:7:alpha beta"));
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("truncated"), Some(&json!(false)));
        assert_eq!(metadata.get("shown"), Some(&json!(1)));
        assert_eq!(metadata.get("total"), Some(&json!(1)));
        assert_eq!(metadata.get("files_with_matches"), Some(&json!(1)));
    }

    #[tokio::test]
    async fn no_content_matches_succeed() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/lib.rs"), "alpha beta\n");

        let prepared = SearchTool
            .prepare(input("gamma", dir.path()))
            .await
            .unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        assert_eq!(result.output, "(no matches)");
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("total"), Some(&json!(0)));
    }

    #[tokio::test]
    async fn prepares_paths_outside_workspace_for_permission_check() {
        let prepared = SearchTool
            .prepare(SearchInput {
                path: Some("/".to_string()),
                ..input("anything", Path::new("."))
            })
            .await
            .unwrap();

        let preview = SearchTool
            .permission_preview(&prepared)
            .expect("search should provide permission preview");
        assert_eq!(prepared.raw_path, Some(PathBuf::from("/")));
        assert!(matches!(
            preview,
            PermissionPreview::Search(SearchPermissionPreview { path, .. }) if path == "/"
        ));
    }

    #[tokio::test]
    async fn default_cap_truncates_output() {
        let dir = TempDir::new();
        let content: String = (1..=150).map(|i| format!("hit {i}\n")).collect();
        write_file(&dir.path().join("many.txt"), &content);

        let prepared = SearchTool.prepare(input("hit", dir.path())).await.unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        assert!(result.output.contains("Found 150 matches"));
        assert!(result.output.contains("(more matches available)"));
        assert!(result.output.contains("hit 1"));
        assert!(result.output.contains("hit 100"));
        assert!(!result.output.contains("hit 101"));
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("truncated"), Some(&json!(true)));
        assert_eq!(metadata.get("shown"), Some(&json!(100)));
        assert_eq!(metadata.get("total"), Some(&json!(150)));
    }

    #[tokio::test]
    async fn files_mode_filters_names_and_honors_include() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/main.rs"), "fn main() {}\n");
        write_file(&dir.path().join("src/main.txt"), "main\n");
        write_file(&dir.path().join("README.md"), "# main\n");

        let mut search = input("main", dir.path());
        search.mode = SearchMode::Files;
        search.include = Some("*.rs".to_string());
        let prepared = SearchTool.prepare(search).await.unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        assert!(result.output.contains("src/main.rs"));
        assert!(!result.output.contains("src/main.txt"));
        assert!(!result.output.contains("README.md"));
    }

    #[tokio::test]
    async fn content_search_groups_multiple_matches_in_one_file() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/lib.rs"), "alpha beta\nalpha gamma\n");

        let prepared = SearchTool
            .prepare(input("alpha", dir.path()))
            .await
            .unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        assert!(result.output.contains("Found 2 matches"));
        assert!(result.output.contains("src/lib.rs:"));
        assert!(result.output.contains("  Line 1: alpha beta"));
        assert!(result.output.contains("  Line 2: alpha gamma"));
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("files_with_matches"), Some(&json!(1)));
        assert_eq!(metadata.get("truncated"), Some(&json!(false)));
        assert_eq!(metadata.get("shown"), Some(&json!(2)));
    }

    #[tokio::test]
    async fn content_search_truncates_long_match_line() {
        let dir = TempDir::new();
        let long = "a".repeat(3000);
        write_file(&dir.path().join("big.txt"), &format!("{long}\n"));

        let prepared = SearchTool.prepare(input("a", dir.path())).await.unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        assert!(result.output.contains("..."));
        assert!(!result.output.contains(&long));
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("truncated"), Some(&json!(false)));
    }

    #[tokio::test]
    async fn content_search_invalid_regex_reports_invalid_pattern() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/lib.rs"), "alpha beta\n");

        let prepared = SearchTool
            .prepare(input("[unclosed", dir.path()))
            .await
            .unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(result.is_error);
        assert!(result.output.contains("rg invalid pattern:"));
    }

    #[tokio::test]
    async fn content_search_metadata_counts_files_with_matches() {
        let dir = TempDir::new();
        write_file(&dir.path().join("a.txt"), "alpha here\n");
        write_file(&dir.path().join("b.txt"), "alpha there\n");

        let prepared = SearchTool
            .prepare(input("alpha", dir.path()))
            .await
            .unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("files_with_matches"), Some(&json!(2)));
    }

    #[tokio::test]
    async fn dot_git_is_excluded_in_both_modes() {
        let dir = TempDir::new();
        // 一个真实项目文件,内含 unique_token
        write_file(&dir.path().join("src/lib.rs"), "alpha unique_token here\n");
        // 模拟 .git 目录结构,内含同样的 token,验证应被排除
        write_file(
            &dir.path().join(".git/HEAD"),
            "ref: refs/heads/unique_token\n",
        );
        write_file(
            &dir.path().join(".git/objects/pack/abc.pack"),
            "PACKunique_token",
        );

        // content 模式:.git 下的两个 unique_token 不应被搜出
        let prepared = SearchTool
            .prepare(input("unique_token", dir.path()))
            .await
            .unwrap();
        let result = execute_for_test(prepared, dir.path()).await;
        assert!(!result.is_error);
        assert!(result.output.contains("src/lib.rs"));
        assert!(!result.output.contains(".git"));
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("files_with_matches"), Some(&json!(1)));

        // files 模式:.git 路径不应出现在列表里
        let mut files_search = input(".git", dir.path());
        files_search.mode = SearchMode::Files;
        let prepared_files = SearchTool.prepare(files_search).await.unwrap();
        let result_files = execute_for_test(prepared_files, dir.path()).await;
        assert!(!result_files.is_error);
        assert!(!result_files.output.contains(".git"));
    }

    #[tokio::test]
    async fn files_mode_metadata_marks_mode_and_counts() {
        let dir = TempDir::new();
        write_file(&dir.path().join("a.rs"), "fn a() {}\n");
        write_file(&dir.path().join("b.rs"), "fn b() {}\n");

        let mut search = input(".rs", dir.path());
        search.mode = SearchMode::Files;
        let prepared = SearchTool.prepare(search).await.unwrap();
        let result = execute_for_test(prepared, dir.path()).await;

        assert!(!result.is_error);
        let metadata = result.metadata.as_ref().expect("metadata present");
        assert_eq!(metadata.get("mode"), Some(&json!("files")));
        assert_eq!(metadata.get("truncated"), Some(&json!(false)));
        assert_eq!(metadata.get("total"), Some(&json!(2)));
        assert_eq!(metadata.get("shown"), Some(&json!(2)));
    }

    #[test]
    fn parse_rg_json_handles_partial_json_and_other_records() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(br#"{"type":"begin","data":{"path":{"text":"src/lib.rs"}}}"#);
        bytes.push(b'\n');
        bytes.extend_from_slice(
            br#"{"type":"match","data":{"path":{"text":"src/lib.rs"},"lines":{"text":"alpha beta\n"},"line_number":1,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
        );
        bytes.push(b'\n');
        bytes.extend_from_slice(
            br#"{"type":"match","data":{"path":{"text":"src/a.txt"},"lines":{"text":"alpha here\n"},"line_number":1,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
        );
        bytes.push(b'\n');
        bytes.extend_from_slice(
            br#"{"type":"match","data":{"path":{"text":"src/b.txt"},"lines":{"text":"alpha there\n"},"line_number":1,"submatches":[{"match":{"text":"alpha"},"start":0,"end":5}]}}"#,
        );
        bytes.push(b'\n');
        bytes.extend_from_slice(
            br#"{"type":"end","data":{"path":{"text":"src/lib.rs"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0"},"searches":1,"searches_with_match":1,"bytes_searched":0,"bytes_printed":0,"matched_lines":1,"matches":1}}}"#,
        );
        bytes.push(b'\n');
        bytes.extend_from_slice(
            br#"{"type":"summary","data":{"elapsed_total":{"human":"0","nanos":0,"secs":0},"stats":{"elapsed":{"secs":0,"nanos":0,"human":"0"},"searches":1,"searches_with_match":1,"bytes_searched":0,"bytes_printed":0,"matched_lines":2,"matches":2}}}"#,
        );
        bytes.push(b'\n');

        let parsed = parse_rg_json(&bytes, 2).expect("parse should succeed");
        assert_eq!(parsed.matches.len(), 3);
        assert_eq!(parsed.files_with_matches, 3);
        assert!(parsed.truncated);

        let parsed_unbounded = parse_rg_json(&bytes, 500).expect("parse should succeed");
        assert_eq!(parsed_unbounded.matches.len(), 3);
        assert_eq!(parsed_unbounded.files_with_matches, 3);
        assert!(!parsed_unbounded.truncated);
    }
}
