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
}

#[derive(Debug, Clone)]
struct InstructionFile {
    path: PathBuf,
    content: String,
}

/// Build the full system prompt for the current request.
pub fn build_system_prompt(settings: &Settings) -> String {
    let env = EnvironmentContext::detect(&settings.cwd);
    let global_instructions = load_global_instructions();
    let project_instructions = load_project_instructions(&settings.cwd);

    let mut prompt = String::new();
    prompt.push_str("You are Omini, a coding agent running in the user's local terminal.\n\n");
    prompt.push_str(&agent_identity_section());
    prompt.push('\n');
    prompt.push_str(&core_behavior_section());
    prompt.push('\n');
    prompt.push_str(&tool_instructions_section());
    prompt.push('\n');
    // TODO: Add a skills section after the skills registry and loading protocol are implemented.
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
            is_git_repo: is_git_repository(cwd),
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
## Shell

- Commands are executed with `sh -c`.
- Run commands relative to the current working directory unless another directory is specified.
- Prefer `rg` for searching text or files.
- Avoid destructive commands unless the user explicitly requested them.

## Git Safety

- Before git write operations, inspect the current repository state.
- Do not use destructive git operations such as `git reset --hard`, forced checkout, or forced clean unless the user explicitly asks for them.
- Do not revert unrelated changes.

## Python

- Use `uv run` instead of invoking `python` or `python3` directly.
</tool_instructions>
"#
    .trim()
    .to_string()
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

fn is_git_repository(cwd: &Path) -> bool {
    cwd.ancestors().any(|path| path.join(".git").exists())
}
