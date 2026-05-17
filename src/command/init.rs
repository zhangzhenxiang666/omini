use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::display::{DisplayMention, DisplayMessage, MentionKind};
use crate::types::events::{CommandEffect, CommandResult};
use crate::types::message::{Message, Role};
use async_trait::async_trait;

const INIT_PROMPT: &str = r#"Analyze this repository and create or update an AGENTS.md file for future omini agents working in this project.

This is a repository-initialization task. Start by using the subagent tool for broad codebase exploration unless that tool is unavailable. The subagent work should be read-only and should report the project shape, common commands, and any important existing instruction files. After the subagent returns, verify the key findings yourself before writing.

What to include in AGENTS.md:
1. Common commands needed to work in this codebase, including build, lint/format, tests, running the app, and how to run a single focused test when applicable.
2. High-level architecture and module relationships that are not obvious from listing files. Focus on the big-picture flow future agents need in order to be productive quickly.
3. Important repository-specific instructions from README.md, existing AGENTS.md, CLAUDE.md, .cursor/rules/, .cursorrules, or .github/copilot-instructions.md if those files exist.

How to write it:
- If AGENTS.md already exists, merge in useful missing information instead of replacing unrelated guidance.
- If only CLAUDE.md exists, use it as source material, but write AGENTS.md for omini agents rather than Claude Code specifically.
- Keep it concise and avoid generic engineering advice.
- Do not include obvious rules such as keeping secrets out of commits or writing helpful error messages.
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
}
