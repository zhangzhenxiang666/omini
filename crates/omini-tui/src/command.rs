use crate::types::events::{CommandKind, CommandSummary};

pub const INIT_PROMPT: &str = r#"Analyze this repository and create or update an AGENTS.md file for future vibe coding agents working in this project.

Treat AGENTS.md as the canonical, tool-agnostic project rules file. Do not create or migrate tool-specific instruction files.

This is a repository-initialization task. Start by using the subagent tool for phased, evidence-focused exploration unless that tool is unavailable. The subagent work should be read-only and should return only AGENTS-relevant facts for the parent agent, not a full project tour. After the subagent returns, verify the key findings yourself before writing.

Phased exploration:
1. First inspect existing AGENTS.md if present, project docs such as README.md, manifests/configs, entrypoints, and a file list.
2. Deep-dive only into files needed to determine common commands, core architecture flow, focused verification commands, and durable project rules.
3. Stop exploring once enough evidence exists to update AGENTS.md confidently.

What to inspect:
1. Existing AGENTS.md, if present.
2. Project docs such as README.md.
3. Manifests and config files such as Cargo.toml, package manifests, test configs, formatter configs, and CI configs when they exist.
4. Source layout and tests needed to understand the main runtime flow and focused verification commands.

Write AGENTS.md with these sections, in this order:
1. Common Commands
2. Architecture Notes
3. Agent Behavior
4. Project-Specific Rules

What to include:
1. Common commands needed to work in this codebase, including build, lint/format, tests, running the app, and how to run a single focused test when applicable.
2. High-level architecture and module relationships that are not obvious from listing files. Focus on the big-picture flow future agents need in order to be productive quickly.
3. Repository-specific instructions discovered from existing AGENTS.md, README.md, manifests, configs, or source conventions.
4. These default Agent Behavior rules:
   - Think before coding: surface assumptions, ambiguity, and tradeoffs before implementation.
   - Simplicity first: implement the smallest solution that satisfies the request; avoid speculative abstractions and unnecessary flexibility.
   - Surgical changes: only edit files and lines directly related to the task; mention unrelated issues instead of fixing them.
   - Goal-driven execution: define success criteria for non-trivial work and verify with focused tests or checks.

How to write it:
- If AGENTS.md already exists, merge in useful missing information instead of replacing unrelated guidance.
- Keep user/project-specific instructions higher priority than generic behavior rules.
- Keep it compact enough to be useful as persistent prompt context. Around 60-90 lines is a good default for many projects, but completeness of durable project rules matters more than hitting a line count. Prefer concise bullets over deleting important rules. Avoid exceeding 120 lines unless the repository genuinely needs it or the user asks for more detail.
- Project-Specific Rules should usually include 6-10 high-value durable rules when the repository has enough evidence, such as testing style, async runtime, error handling, tool registration, permission config, subagent definitions, comment language, and verification expectations.
- Do not include obvious rules such as keeping secrets out of commits or writing helpful error messages.
- Do not include volatile details such as test counts, command timings, file line counts, exhaustive module inventories, directory trees, or project-encyclopedia content.
- Path and storage facts must be exact. If you cannot verify a path from code or config, write `unknown` or omit it instead of generalizing.
- Do not include generic language, framework, or ecosystem facts unless they are a project-specific rule agents must follow in this repository.
- Do not invent sections like "Common Development Tasks", "Tips", or "Support" unless they are grounded in files you inspected.
- Prefer concrete commands and concrete architecture notes over broad descriptions.

Before finishing, report whether AGENTS.md was created or updated and summarize the most important changes."#;

pub fn builtin_command_summaries() -> Vec<CommandSummary> {
    vec![
        builtin("sessions", &["resume"], "切换会话", 10, false, None),
        builtin(
            "new",
            &["clear"],
            "清空当前会话，开始新对话",
            20,
            false,
            None,
        ),
        builtin("plan", &[], "切换到 plan mode", 25, false, None),
        builtin(
            "compact",
            &[],
            "压缩当前会话上下文",
            30,
            true,
            Some("[custom summarization instructions]"),
        ),
        builtin("model", &[], "切换模型", 30, false, None),
        builtin("agents", &[], "管理 agent", 35, false, None),
        builtin(
            "effort",
            &[],
            "调整当前模型的思考程度",
            40,
            true,
            Some("<none | low | medium | high>"),
        ),
        builtin("init", &[], "分析项目并生成 AGENTS.md", 50, true, None),
        builtin("rename", &[], "重命名当前会话", 60, true, Some("<name>")),
        builtin(
            "thinking",
            &[],
            "开启/关闭消息区 thinking 块展示",
            80,
            true,
            Some("[on | off]"),
        ),
        builtin("help", &["?"], "显示帮助", 900, false, None),
        builtin("exit", &["quit"], "退出程序", 1000, false, None),
    ]
}

pub fn commands_with_runtime_skills(runtime_commands: Vec<CommandSummary>) -> Vec<CommandSummary> {
    let mut commands = builtin_command_summaries();
    commands.extend(
        runtime_commands
            .into_iter()
            .filter(|command| command.kind == CommandKind::Skill),
    );
    commands.sort_by(|a, b| {
        a.sort_weight
            .cmp(&b.sort_weight)
            .then_with(|| a.name.cmp(&b.name))
    });
    commands
}

fn builtin(
    name: &str,
    aliases: &[&str],
    description: &str,
    sort_weight: i32,
    has_args: bool,
    args_description: Option<&str>,
) -> CommandSummary {
    CommandSummary {
        name: name.to_string(),
        aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
        description: description.to_string(),
        sort_weight,
        kind: CommandKind::Builtin,
        has_args,
        args_description: args_description.map(str::to_string),
    }
}
