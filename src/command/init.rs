use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::display::{DisplayMention, DisplayMessage, MentionKind};
use crate::types::events::{CommandEffect, CommandResult};
use crate::types::message::{Message, Role};
use async_trait::async_trait;

const INIT_PROMPT: &str = r#"Analyze this repository and create or update an AGENTS.md file for future vibe coding agents working in this project.

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

fn build_init_query(args: &str, description: &str) -> (Message, DisplayMessage) {
    let mut prompt = INIT_PROMPT.to_string();
    let args = args.trim();
    if !args.is_empty() {
        prompt.push_str("\n\nAdditional user notes for this initialization:\n");
        prompt.push_str(args);
    }
    let display_text = if args.is_empty() {
        "/init".to_string()
    } else {
        format!("/init {args}")
    };

    (
        Message::from_user_text(prompt),
        DisplayMessage {
            role: Role::User,
            text: display_text,
            mentions: vec![DisplayMention {
                start_char: 0,
                end_char: 5,
                kind: MentionKind::Command,
                label: "init".to_string(),
                target: "init".to_string(),
                description: description.to_string(),
            }],
        },
    )
}

pub struct InitCommand;

#[async_trait]
impl Command for InitCommand {
    fn name(&self) -> &'static str {
        "init"
    }

    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    fn description(&self) -> &'static str {
        "分析项目并生成 AGENTS.md"
    }

    fn sort_weight(&self) -> i32 {
        50
    }

    async fn execute(&self, _runtime: &mut AgentRuntime, args: &str) -> CommandResult {
        let (llm_message, display_message) = build_init_query(args, self.description());
        CommandResult::Ok(vec![CommandEffect::inject_user_query(
            llm_message,
            display_message,
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::ContentBlock;

    #[test]
    fn init_query_uses_display_message_for_command_echo() {
        let (llm_message, display_message) = build_init_query("focus on tests", "description");
        assert_eq!(display_message.text, "/init focus on tests");
        assert_eq!(display_message.mentions[0].kind, MentionKind::Command);
        assert_eq!(display_message.mentions[0].start_char, 0);
        assert_eq!(display_message.mentions[0].end_char, 5);

        let llm_text = llm_message
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(llm_text.contains(INIT_PROMPT));
        assert!(llm_text.contains("Additional user notes"));
        assert!(llm_text.contains("focus on tests"));
    }

    #[test]
    fn init_query_treats_agents_as_canonical_rules_file() {
        let (llm_message, _) = build_init_query("", "description");
        let llm_text = llm_message
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .unwrap();

        assert!(llm_text.contains("Treat AGENTS.md as the canonical"));
        assert!(llm_text.contains("Common Commands"));
        assert!(llm_text.contains("Architecture Notes"));
        assert!(llm_text.contains("Agent Behavior"));
        assert!(llm_text.contains("Project-Specific Rules"));
        assert!(llm_text.contains("Think before coding"));
        assert!(llm_text.contains("Simplicity first"));
        assert!(llm_text.contains("Surgical changes"));
        assert!(llm_text.contains("Goal-driven execution"));
        assert!(llm_text.contains("If AGENTS.md already exists, merge"));
    }

    #[test]
    fn init_query_requires_phased_balanced_initialization() {
        let (llm_message, _) = build_init_query("", "description");
        let llm_text = llm_message
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .unwrap();

        assert!(llm_text.contains("phased, evidence-focused exploration"));
        assert!(llm_text.contains("not a full project tour"));
        assert!(llm_text.contains("Deep-dive only into files needed"));
        assert!(llm_text.contains("Stop exploring once enough evidence exists"));
        assert!(llm_text.contains("Around 60-90 lines is a good default"));
        assert!(llm_text.contains("durable project rules matters more"));
        assert!(llm_text.contains("Prefer concise bullets over deleting important rules"));
        assert!(llm_text.contains("Avoid exceeding 120 lines"));
        assert!(llm_text.contains("6-10 high-value durable rules"));
        assert!(llm_text.contains("testing style"));
        assert!(llm_text.contains("tool registration"));
        assert!(llm_text.contains("subagent definitions"));
        assert!(llm_text.contains("volatile details"));
        assert!(llm_text.contains("directory trees"));
        assert!(llm_text.contains("project-encyclopedia content"));
    }

    #[test]
    fn init_query_requires_exact_project_specific_facts() {
        let (llm_message, _) = build_init_query("", "description");
        let llm_text = llm_message
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .unwrap();

        assert!(llm_text.contains("Path and storage facts must be exact"));
        assert!(llm_text.contains("write `unknown` or omit it"));
        assert!(llm_text.contains("instead of generalizing"));
        assert!(llm_text.contains("Do not include generic language"));
        assert!(llm_text.contains("ecosystem facts"));
        assert!(llm_text.contains("project-specific rule agents must follow"));
    }

    #[test]
    fn init_query_does_not_reference_tool_specific_rule_files() {
        let (llm_message, _) = build_init_query("", "description");
        let llm_text = llm_message
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .unwrap();

        assert!(!llm_text.contains("CLAUDE.md"));
        assert!(!llm_text.contains(".cursor/rules"));
        assert!(!llm_text.contains(".cursorrules"));
        assert!(!llm_text.contains("copilot-instructions.md"));
        assert!(!llm_text.contains("Claude Code"));
    }
}
