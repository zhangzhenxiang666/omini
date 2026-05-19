use super::{Tool, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_RESULTS_LIMIT: usize = 500;
const MAX_CONTEXT_LINES: usize = 10;
const OUTPUT_BYTE_LIMIT: usize = 256 * 1024;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);

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
    /// Include/exclude globs passed to rg --glob. Prefix excludes with !.
    #[serde(default)]
    pub globs: Vec<String>,
    /// Case-sensitive matching. Defaults to true.
    #[serde(default = "default_case_sensitive")]
    pub case_sensitive: bool,
    /// Treat query as a literal string instead of a regex. Defaults to false.
    #[serde(default)]
    pub literal: bool,
    /// Context lines around content matches. Defaults to 0, capped at 10.
    #[serde(default)]
    pub context: Option<usize>,
    /// Maximum result lines to return. Defaults to 100, capped at 500.
    #[serde(default)]
    pub max_results: Option<usize>,
}

fn default_case_sensitive() -> bool {
    true
}

pub struct SearchTool;

#[derive(Debug)]
pub struct PreparedSearch {
    mode: SearchMode,
    query: String,
    cwd: PathBuf,
    path_arg: OsString,
    globs: Vec<String>,
    case_sensitive: bool,
    literal: bool,
    context: usize,
    max_results: usize,
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
            "  {\"query\":\"struct .*Tool\",\"globs\":[\"*.rs\"]}\n",
            "\n",
            "Input:\n",
            "  query           Regex or literal text to search for. Required for content mode.\n",
            "  mode            `content` to search file contents, `files` to search file paths.\n",
            "  path            File or directory to search. Defaults to current working directory.\n",
            "  globs           Optional rg globs, including ! excludes.\n",
            "  case_sensitive  Optional bool. Defaults to true.\n",
            "  literal         Optional bool. Defaults to false.\n",
            "  context         Optional content context lines, capped at 10.\n",
            "  max_results     Optional returned result line limit, capped at 500.\n",
            "\n",
            "Rules:\n",
            "  - Search is read-only and limited to the current workspace or /tmp.\n",
            "  - Use `read` after search when you need a larger code window."
        )
    }

    async fn prepare(&self, input: SearchInput) -> Result<Self::Prepared, ToolResult> {
        prepare_search(input).map_err(ToolResult::error)
    }

    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        _ctx: ToolExecutionContext,
    ) -> ToolResult {
        execute_search(prepared).await
    }
}

fn prepare_search(input: SearchInput) -> Result<PreparedSearch, String> {
    if input.mode == SearchMode::Content && input.query.trim().is_empty() {
        return Err("query must not be empty in content mode".to_string());
    }

    let cwd = std::env::current_dir()
        .map_err(|e| format!("Failed to determine current working directory: {e}"))?
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize current working directory: {e}"))?;
    let requested_path = input.path.as_deref().unwrap_or(".").trim();
    let requested_path = if requested_path.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(requested_path)
    };
    let absolute_path = if requested_path.is_absolute() {
        requested_path
    } else {
        cwd.join(requested_path)
    };
    let search_path = absolute_path
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize search path: {e}"))?;

    if !is_allowed_search_path(&cwd, &search_path) {
        return Err(format!(
            "search path must be inside the current workspace or /tmp: {}",
            search_path.display()
        ));
    }
    if is_private_path(&search_path) {
        return Err(format!(
            "refusing to search private path with default-allowed search: {}",
            search_path.display()
        ));
    }
    if input.mode == SearchMode::Files && !search_path.is_dir() {
        return Err("files mode requires path to be a directory".to_string());
    }

    let path_arg = command_path_arg(&cwd, &search_path);
    let globs = input
        .globs
        .into_iter()
        .map(|glob| glob.trim().to_string())
        .filter(|glob| !glob.is_empty())
        .collect();
    let context = input.context.unwrap_or(0).min(MAX_CONTEXT_LINES);
    let max_results = input
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, MAX_RESULTS_LIMIT);

    Ok(PreparedSearch {
        mode: input.mode,
        query: input.query,
        cwd,
        path_arg,
        globs,
        case_sensitive: input.case_sensitive,
        literal: input.literal,
        context,
        max_results,
    })
}

fn is_allowed_search_path(cwd: &Path, path: &Path) -> bool {
    path.starts_with(cwd) || path.starts_with("/tmp")
}

fn is_private_path(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        name == ".env"
            || name.starts_with(".env.")
            || name == ".ssh"
            || name.contains("token")
            || name.contains("secret")
            || name.contains("credential")
    })
}

fn command_path_arg(cwd: &Path, path: &Path) -> OsString {
    if let Ok(relative) = path.strip_prefix(cwd) {
        if relative.as_os_str().is_empty() {
            return OsString::from(".");
        }
        return relative.as_os_str().to_os_string();
    }
    path.as_os_str().to_os_string()
}

async fn execute_search(prepared: PreparedSearch) -> ToolResult {
    let output = match run_rg(&prepared).await {
        Ok(output) => output,
        Err(e) => return ToolResult::error(e),
    };

    if !output.status.success() {
        if prepared.mode == SearchMode::Content && output.status.code() == Some(1) {
            return ToolResult::ok("(no matches)");
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

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = match prepared.mode {
        SearchMode::Content => stdout.lines().map(ToOwned::to_owned).collect(),
        SearchMode::Files => filter_file_lines(&stdout, &prepared),
    };

    format_results(lines, prepared.max_results)
}

async fn run_rg(prepared: &PreparedSearch) -> Result<std::process::Output, String> {
    let mut command = Command::new("rg");
    command.current_dir(&prepared.cwd);
    command.kill_on_drop(true);

    match prepared.mode {
        SearchMode::Content => {
            command.args([
                "--line-number",
                "--column",
                "--no-heading",
                "--color",
                "never",
            ]);
            if prepared.context > 0 {
                command.arg("--context").arg(prepared.context.to_string());
            }
            if !prepared.case_sensitive {
                command.arg("--ignore-case");
            }
            if prepared.literal {
                command.arg("--fixed-strings");
            }
            for glob in &prepared.globs {
                command.arg("--glob").arg(glob);
            }
            command.arg("--");
            command.arg(&prepared.query);
            command.arg(&prepared.path_arg);
        }
        SearchMode::Files => {
            command.args(["--files", "--color", "never"]);
            for glob in &prepared.globs {
                command.arg("--glob").arg(glob);
            }
            command.arg(&prepared.path_arg);
        }
    }

    match tokio::time::timeout(SEARCH_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("ripgrep (`rg`) was not found in PATH".to_string())
        }
        Ok(Err(e)) => Err(format!("Failed to spawn rg: {e}")),
        Err(_) => Err(format!(
            "search timed out after {}ms",
            SEARCH_TIMEOUT.as_millis()
        )),
    }
}

fn filter_file_lines(stdout: &str, prepared: &PreparedSearch) -> Vec<String> {
    let query = prepared.query.trim();
    if query.is_empty() {
        return stdout.lines().map(ToOwned::to_owned).collect();
    }

    if prepared.case_sensitive {
        stdout
            .lines()
            .filter(|line| line.contains(query))
            .map(ToOwned::to_owned)
            .collect()
    } else {
        let query = query.to_ascii_lowercase();
        stdout
            .lines()
            .filter(|line| line.to_ascii_lowercase().contains(&query))
            .map(ToOwned::to_owned)
            .collect()
    }
}

fn format_results(lines: Vec<String>, max_results: usize) -> ToolResult {
    if lines.is_empty() {
        return ToolResult::ok("(no matches)");
    }

    let total = lines.len();
    let mut output = String::new();
    let mut shown = 0usize;

    for line in lines.into_iter().take(max_results) {
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

    ToolResult::ok(output.trim_end().to_string())
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
            globs: Vec::new(),
            case_sensitive: true,
            literal: false,
            context: None,
            max_results: None,
        }
    }

    #[tokio::test]
    async fn content_search_finds_file_line_and_column() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/lib.rs"), "alpha beta\n");

        let prepared = SearchTool.prepare(input("beta", dir.path())).await.unwrap();
        let result = SearchTool
            .execute_prepared(prepared, ToolExecutionContext::test("search"))
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("src/lib.rs:1:7:alpha beta"));
    }

    #[tokio::test]
    async fn no_content_matches_succeed() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/lib.rs"), "alpha beta\n");

        let prepared = SearchTool
            .prepare(input("gamma", dir.path()))
            .await
            .unwrap();
        let result = SearchTool
            .execute_prepared(prepared, ToolExecutionContext::test("search"))
            .await;

        assert!(!result.is_error);
        assert_eq!(result.output, "(no matches)");
    }

    #[tokio::test]
    async fn rejects_paths_outside_workspace_and_tmp() {
        let err = SearchTool
            .prepare(SearchInput {
                path: Some("/".to_string()),
                ..input("anything", Path::new("."))
            })
            .await
            .unwrap_err();

        assert!(err.output.contains("current workspace or /tmp"));
    }

    #[tokio::test]
    async fn max_results_truncates_output() {
        let dir = TempDir::new();
        write_file(&dir.path().join("many.txt"), "hit 1\nhit 2\nhit 3\n");
        let mut search = input("hit", dir.path());
        search.max_results = Some(2);

        let prepared = SearchTool.prepare(search).await.unwrap();
        let result = SearchTool
            .execute_prepared(prepared, ToolExecutionContext::test("search"))
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("hit 1"));
        assert!(result.output.contains("hit 2"));
        assert!(!result.output.contains("hit 3"));
        assert!(result.output.contains("output truncated"));
    }

    #[tokio::test]
    async fn files_mode_filters_names_and_honors_globs() {
        let dir = TempDir::new();
        write_file(&dir.path().join("src/main.rs"), "fn main() {}\n");
        write_file(&dir.path().join("src/main.txt"), "main\n");
        write_file(&dir.path().join("README.md"), "# main\n");

        let mut search = input("main", dir.path());
        search.mode = SearchMode::Files;
        search.globs = vec!["*.rs".to_string()];
        let prepared = SearchTool.prepare(search).await.unwrap();
        let result = SearchTool
            .execute_prepared(prepared, ToolExecutionContext::test("search"))
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("src/main.rs"));
        assert!(!result.output.contains("src/main.txt"));
        assert!(!result.output.contains("README.md"));
    }
}
