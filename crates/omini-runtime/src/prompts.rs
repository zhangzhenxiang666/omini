use crate::subagents::AgentSummary;
use crate::types::config::Settings;
use chrono::Local;
use std::path::{Path, PathBuf};

const COMMAND_SHELL: &str = "sh -c";

#[derive(Debug, Clone)]
struct EnvironmentContext {
    cwd: PathBuf,
    command_shell: String,
    login_shell: Option<String>,
    current_date: String,
    timezone: String,
    platform: String,
    os: Option<String>,
    kernel: Option<String>,
    architecture: String,
    is_git_repo: bool,
    git_branch: Option<String>,
}

#[derive(Debug, Clone)]
struct InstructionFile {
    path: PathBuf,
    content: String,
}

/// Build the full system prompt for the current request.
pub fn build_system_prompt(settings: &Settings) -> String {
    let subagents = crate::subagents::load_agent_summaries(&settings.cwd);
    build_system_prompt_with_subagents(settings, &subagents)
}

/// Build the full system prompt with a runtime-provided capability snapshot.
pub fn build_system_prompt_with_subagents(
    settings: &Settings,
    subagents: &[AgentSummary],
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are Omini, a coding agent running in the user's local terminal.\n\n");
    prompt.push_str(&agent_identity_section());
    prompt.push('\n');
    prompt.push_str(&core_behavior_section());
    prompt.push('\n');
    prompt.push_str(&tool_instructions_section());
    prompt.push('\n');
    if let Some(section) = language_preference_section(settings) {
        prompt.push_str(&section);
        prompt.push('\n');
    }
    prompt.push_str(&subagent_section(subagents));
    prompt.push('\n');
    // TODO: Add a skills section after the skills registry and loading protocol are implemented.
    prompt.push_str(&project_context_prompt(&settings.cwd));
    prompt
}

pub fn language_preference_section(settings: &Settings) -> Option<String> {
    let language = settings.language.as_deref()?.trim();
    if language.is_empty() {
        return None;
    }

    Some(format!(
        r#"<language_preference>
- Use `{language}` for user-facing responses unless the latest user request asks for another language.
</language_preference>"#
    ))
}

pub fn project_context_prompt(cwd: &Path) -> String {
    let env = EnvironmentContext::detect(cwd);
    let global_instructions = load_global_instructions();
    let project_instructions = load_project_instructions(cwd);

    let mut prompt = String::new();
    prompt.push_str(&project_instructions_section(
        global_instructions.as_ref(),
        project_instructions.as_ref(),
    ));
    prompt.push('\n');
    prompt.push_str(&environment_context_section(&env));
    prompt
}

impl EnvironmentContext {
    fn detect(cwd: &Path) -> Self {
        let git_metadata_dir = git_metadata_dir(cwd);
        Self {
            cwd: cwd.to_path_buf(),
            command_shell: COMMAND_SHELL.to_string(),
            login_shell: non_empty_env("SHELL"),
            current_date: Local::now().format("%Y-%m-%d").to_string(),
            timezone: detect_timezone(),
            platform: std::env::consts::OS.to_string(),
            os: detect_os_pretty_name(),
            kernel: detect_kernel(),
            architecture: std::env::consts::ARCH.to_string(),
            is_git_repo: git_metadata_dir.is_some(),
            git_branch: git_metadata_dir.as_deref().and_then(detect_git_branch),
        }
    }
}

fn agent_identity_section() -> String {
    r#"<agent_identity>
## Role

- You are a pragmatic software engineering agent.
- You help the user inspect, modify, test, and explain code in the current workspace.
- You operate through the available tools and the local terminal environment.

## Priorities

- Follow the user's latest request.
- Respect project instructions and existing code conventions.
- Prefer small, focused changes over broad rewrites.
- Verify meaningful changes when a reasonable verification path exists.
</agent_identity>
"#
    .trim()
    .to_string()
}

fn core_behavior_section() -> String {
    r#"<core_behavior>
## Working Style

- Read the relevant code before making non-trivial changes.
- Prefer existing project patterns, dependencies, and naming conventions.
- Keep edits scoped to the task.
- Do not rewrite unrelated code.
- Do not discard or overwrite user changes unless the user explicitly asks for it.

## Task Routing

- Treat requests to "explore", "introduce", "explain", "summarize", "walk me through", or "help me understand" the current project as broad codebase exploration unless the user names a specific file or symbol.
- For broad codebase exploration, first use the `subagent` tool with the `explorer` subagent, then synthesize its findings for the user.
- Keep only targeted follow-up inspection in the main context after the explorer returns.

## Communication

- Be direct and concise.
- Explain important assumptions and tradeoffs.
- If blocked, state the blocker and the next practical option.
- Use the user's language unless there is a clear reason to do otherwise.

## Code Editing

- Use the available file editing tool whenever possible.
- Preserve formatting style already used by the file.
- Add comments only when they clarify non-obvious logic.

## Verification

- Run targeted tests, builds, formatters, or checks when they are relevant and available.
- If verification cannot be run, explain why.
</core_behavior>
"#
    .trim()
    .to_string()
}

fn tool_instructions_section() -> String {
    r#"<tool_instructions>
## Search

- Use the `search` tool for local file content search and filename lookup.
- Use `read` after search when you need a larger code window.
- Prefer `search` over `bash` for project exploration, symbol lookup, file discovery, and code matching.
- Use shell search commands only when the user explicitly asks for a shell command or the `search` tool cannot express the needed query. Briefly explain why.

## Shell

- Commands are executed with `sh -c`.
- Run commands relative to the current working directory unless another directory is specified.
- Avoid destructive commands unless the user explicitly requested them.

## Git Safety

- Before git write operations, inspect the current repository state.
- Do not use destructive git operations such as `git reset --hard`, forced checkout, or forced clean unless the user explicitly asks for them.
- Do not revert unrelated changes.
</tool_instructions>
"#
    .trim()
    .to_string()
}

fn subagent_section(agents: &[AgentSummary]) -> String {
    let mut section = String::new();
    section.push_str("<delegation_instructions>\n");
    section.push_str("## Delegation Policy\n\n");
    section.push_str(
        "- Subagents isolate their intermediate context from the main conversation and return only a final result.\n",
    );
    section.push_str(
        "- Use the `subagent` tool proactively when isolation, parallelism, or specialized exploration materially helps the task.\n",
    );
    section.push_str(
        "- For broad codebase exploration, architecture discovery, dependency tracing, project introductions, project overviews, or research likely to require more than 3 searches or file reads, use the `explorer` subagent instead of doing all exploration in the main context.\n",
    );
    section.push_str(
        "- For multiple independent codebase questions, run multiple `explorer` subagents in the same assistant turn when practical.\n",
    );
    section.push_str(
        "- For focused implementation work that can be separated by file or module ownership, use `worker` subagents; give each worker a bounded scope and explicit files or responsibilities.\n",
    );
    section.push_str(
        "- Do not duplicate a subagent's investigation in the main context. Use the subagent result as input, then inspect only the specific files needed to integrate, verify, or resolve uncertainty.\n",
    );
    section.push_str(
        "- Keep urgent blocking work local when the next step cannot proceed without your own immediate inspection or judgment.\n",
    );
    section.push_str(
        "- The main agent remains responsible for synthesis, final decisions, user communication, and verifying code changes before reporting completion.\n\n",
    );
    section.push_str("## Subagent Prompting\n\n");
    section.push_str("- The tool input must include `name` and a concrete `prompt` task.\n");
    section.push_str(
        "- Optionally include a short `title` when a compact UI label would help the user distinguish concurrent subagent tasks. Keep it brief and use the user's language.\n",
    );
    section.push_str(
        "- Write prompts as self-contained briefs: goal, relevant context already known, exact question or expected output, and any limits such as read-only or files to own.\n",
    );
    section.push_str(
        "- Prefer assigning questions over step-by-step command scripts for investigations, so the subagent can adapt if the first search path is wrong.\n",
    );
    section.push_str(
        "- Subagents cannot spawn other subagents. Do not ask them to delegate further.\n\n",
    );
    section.push_str("## Available Subagents\n\n");
    for agent in agents {
        section.push_str(&format!("- `{}`: {}\n", agent.name, agent.description));
    }
    section.push_str("</delegation_instructions>");
    section
}

fn project_instructions_section(
    global: Option<&InstructionFile>,
    project: Option<&InstructionFile>,
) -> String {
    let mut section = String::new();
    section.push_str("<project_instructions>\n");
    section.push_str("## Priority\n\n");
    section.push_str(
        "- Project `AGENTS.md` instructions override global `~/.omini/AGENTS.md` instructions.\n",
    );
    section.push_str(
        "- Apply the most specific instruction when multiple instruction sources overlap.\n",
    );
    section.push_str("- If project instructions conflict with the user's latest request, explain the conflict before proceeding.\n\n");

    match global {
        Some(file) => {
            section.push_str("## Global Instructions\n\n");
            append_instruction_file(&mut section, file);
            section.push('\n');
        }
        None => {
            section.push_str("## Global Instructions\n\n");
            section.push_str("- No global `~/.omini/AGENTS.md` file was found.\n\n");
        }
    }

    match project {
        Some(file) => {
            section.push_str("## Project Instructions\n\n");
            append_instruction_file(&mut section, file);
        }
        None => {
            section.push_str("## Project Instructions\n\n");
            section.push_str(
                "- No project `AGENTS.md` file was found in the current working directory.\n",
            );
        }
    }

    section.push_str("</project_instructions>");
    section
}

fn append_instruction_file(section: &mut String, file: &InstructionFile) {
    section.push_str(&format!("Source: `{}`\n\n", file.path.display()));
    section.push_str("```text\n");
    section.push_str(file.content.trim());
    section.push_str("\n```\n");
}

fn environment_context_section(env: &EnvironmentContext) -> String {
    let mut section = String::new();
    section.push_str("<environment_context>\n");
    section.push_str("## Runtime\n\n");
    section.push_str(&format!("- Working directory: `{}`\n", env.cwd.display()));
    section.push_str(&format!("- Command shell: `{}`\n", env.command_shell));
    if let Some(shell) = &env.login_shell {
        section.push_str(&format!("- Login shell: `{shell}`\n"));
    } else {
        section.push_str("- Login shell: `unknown`\n");
    }
    section.push_str(&format!("- Current date: `{}`\n", env.current_date));
    section.push_str(&format!("- Timezone: `{}`\n", env.timezone));
    section.push_str(&format!("- Platform: `{}`\n", env.platform));
    if let Some(os) = &env.os {
        section.push_str(&format!("- OS: `{os}`\n"));
    } else {
        section.push_str("- OS: `unknown`\n");
    }
    if let Some(kernel) = &env.kernel {
        section.push_str(&format!("- Kernel: `{kernel}`\n"));
    } else {
        section.push_str("- Kernel: `unknown`\n");
    }
    section.push_str(&format!("- Architecture: `{}`\n", env.architecture));
    section.push_str(&format!("- Git repository: `{}`\n", env.is_git_repo));
    if env.is_git_repo {
        if let Some(branch) = &env.git_branch {
            section.push_str(&format!("- Git branch: `{branch}`\n"));
        } else {
            section.push_str("- Git branch: `unknown`\n");
        }
    }
    section.push_str("\n## Notes\n\n");
    section.push_str("- Paths are local filesystem paths.\n");
    section.push_str("- Commands run relative to the working directory unless stated otherwise.\n");
    section.push_str(
        "- This prompt does not assume a sandbox, approval system, or isolated filesystem.\n",
    );
    section.push_str("</environment_context>");
    section
}

fn load_global_instructions() -> Option<InstructionFile> {
    let path = dirs::home_dir()?.join(".omini").join("AGENTS.md");
    load_instruction_file(path)
}

fn load_project_instructions(cwd: &Path) -> Option<InstructionFile> {
    load_instruction_file(cwd.join("AGENTS.md"))
}

fn load_instruction_file(path: PathBuf) -> Option<InstructionFile> {
    let content = std::fs::read_to_string(&path).ok()?;
    let content = content.trim().to_string();
    if content.is_empty() {
        return None;
    }
    Some(InstructionFile { path, content })
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn detect_timezone() -> String {
    if let Some(tz) = non_empty_env("TZ") {
        return tz;
    }

    if let Ok(timezone) = std::fs::read_to_string("/etc/timezone") {
        let timezone = timezone.trim();
        if !timezone.is_empty() {
            return timezone.to_string();
        }
    }

    "unknown".to_string()
}

fn detect_os_pretty_name() -> Option<String> {
    let content = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in content.lines() {
        let Some(value) = line.strip_prefix("PRETTY_NAME=") else {
            continue;
        };
        return Some(unquote_os_release_value(value));
    }
    None
}

fn unquote_os_release_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn detect_kernel() -> Option<String> {
    let os = std::fs::read_to_string("/proc/sys/kernel/ostype").ok()?;
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    let os = os.trim();
    let release = release.trim();
    if os.is_empty() || release.is_empty() {
        return None;
    }
    Some(format!("{os} {release}"))
}

fn git_metadata_dir(cwd: &Path) -> Option<PathBuf> {
    for path in cwd.ancestors() {
        let dot_git = path.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file()
            && let Some(git_dir) = read_gitdir_file(path, &dot_git)
        {
            return Some(git_dir);
        }
    }
    None
}

fn read_gitdir_file(worktree_root: &Path, dot_git: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = content.trim().strip_prefix("gitdir:")?.trim();
    if gitdir.is_empty() {
        return None;
    }
    let gitdir = PathBuf::from(gitdir);
    Some(if gitdir.is_absolute() {
        gitdir
    } else {
        worktree_root.join(gitdir)
    })
}

fn detect_git_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref: ") {
        return Some(
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_string(),
        );
    }
    if head.len() >= 7 {
        return Some(format!("detached {}", &head[..7]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::config::{ProviderType, Settings};
    use std::collections::HashMap;
    use std::fs;
    use uuid::Uuid;

    fn test_settings(language: Option<&str>) -> Settings {
        Settings {
            api_key: "test-key".to_string(),
            base_url: "https://openai.example".to_string(),
            model: "gpt-test".to_string(),
            endpoint: ProviderType::OpenAI,
            providers: HashMap::new(),
            active_provider: "openai".to_string(),
            system_prompt: None,
            language: language.map(str::to_string),
            max_turns: None,
            cwd: std::env::temp_dir(),
            thinking_effort: None,
            permissions: None,
        }
    }

    fn temp_prompt_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("omini-prompt-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create temp prompt dir");
        path
    }

    #[test]
    fn language_preference_section_trims_for_prompt_only() {
        let settings = test_settings(Some("  en  "));

        let section = language_preference_section(&settings).unwrap();

        assert!(section.contains("`en`"));
        assert!(!section.contains("`  en  `"));
    }

    #[test]
    fn language_preference_section_omits_blank_values() {
        let settings = test_settings(Some("   "));

        assert!(language_preference_section(&settings).is_none());
    }

    #[test]
    fn main_system_prompt_includes_language_preference_when_configured() {
        let settings = test_settings(Some("简体中文"));

        let prompt = build_system_prompt_with_subagents(&settings, &[]);

        assert!(prompt.contains("<language_preference>"));
        assert!(prompt.contains("`简体中文`"));
    }

    #[test]
    fn main_system_prompt_omits_language_preference_when_unset() {
        let settings = test_settings(None);

        let prompt = build_system_prompt_with_subagents(&settings, &[]);

        assert!(!prompt.contains("<language_preference>"));
    }

    #[test]
    fn tool_instructions_do_not_hardcode_python_runner_policy() {
        let section = tool_instructions_section();

        assert!(!section.contains("uv run"));
        assert!(!section.contains("python3"));
    }

    #[test]
    fn environment_context_includes_git_branch_when_available() {
        let dir = temp_prompt_dir();
        let git_dir = dir.join(".git");
        fs::create_dir_all(&git_dir).expect("create .git dir");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/search\n").expect("write HEAD");

        let env = EnvironmentContext::detect(&dir);
        let section = environment_context_section(&env);

        assert!(section.contains("- Git repository: `true`"));
        assert!(section.contains("- Git branch: `feature/search`"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn environment_context_reads_worktree_gitdir_file() {
        let dir = temp_prompt_dir();
        let git_dir = dir.join(".git-worktree");
        fs::create_dir_all(&git_dir).expect("create worktree git dir");
        fs::write(dir.join(".git"), "gitdir: .git-worktree\n").expect("write .git file");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/worktree-branch\n").expect("write HEAD");

        let env = EnvironmentContext::detect(&dir);

        assert_eq!(env.git_branch.as_deref(), Some("worktree-branch"));
        let _ = fs::remove_dir_all(dir);
    }
}
