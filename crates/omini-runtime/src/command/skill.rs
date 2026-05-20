use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::skills::SkillSpec;
use crate::types::display::{DisplayMention, DisplayMessage, MentionKind};
use crate::types::events::{CommandEffect, CommandKind, CommandResult};
use crate::types::message::Message;
use crate::types::message::Role;
use async_trait::async_trait;

pub struct SkillCommand {
    spec: SkillSpec,
}

impl SkillCommand {
    pub(crate) fn new(spec: SkillSpec) -> Self {
        Self { spec }
    }
}

fn build_skill_query(spec: &SkillSpec, args: &str) -> (Message, DisplayMessage) {
    let prompt = args.trim();
    let command_text = format!("/{}", spec.name);
    let llm_text = crate::skills::render_skill_slash_command_invocation(spec, Some(prompt));
    let display_text = if prompt.is_empty() {
        command_text.clone()
    } else {
        format!("{command_text} {prompt}")
    };

    let command_end = command_text.chars().count();
    (
        Message::from_user_text(llm_text),
        DisplayMessage {
            role: Role::User,
            text: display_text,
            mentions: vec![DisplayMention {
                start_char: 0,
                end_char: command_end,
                kind: MentionKind::Command,
                label: spec.name.clone(),
                target: spec.name.clone(),
                description: spec.description.clone(),
            }],
        },
    )
}

#[async_trait]
impl Command for SkillCommand {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn has_args(&self) -> bool {
        true
    }

    fn args_description(&self) -> Option<&'static str> {
        Some("[prompt]")
    }

    fn sort_weight(&self) -> i32 {
        500
    }

    fn kind(&self) -> CommandKind {
        CommandKind::Skill
    }

    async fn execute(&self, _runtime: &mut AgentRuntime, args: &str) -> CommandResult {
        let (llm_message, display_message) = build_skill_query(&self.spec, args);
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
    fn skill_display_message_highlights_command_name() {
        let cwd =
            std::env::temp_dir().join(format!("omini-skill-command-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let registry = crate::skills::load_skill_registry(&cwd);
        let spec = registry.get("commit-message").unwrap();
        let (llm_message, display) = build_skill_query(spec, "split the staged diff");

        assert_eq!(display.text, "/commit-message split the staged diff");
        assert_eq!(display.mentions.len(), 1);
        assert_eq!(display.mentions[0].start_char, 0);
        assert_eq!(
            display.mentions[0].end_char,
            "/commit-message".chars().count()
        );
        assert_eq!(display.mentions[0].kind, MentionKind::Command);
        assert_eq!(display.mentions[0].label, "commit-message");
        let ContentBlock::Text(text) = &llm_message.content[0] else {
            panic!("skill command should inject a text block");
        };
        assert!(text.text.contains("<skill_invocation>"));
        assert!(text.text.contains("<source>slash_command</source>"));
        let _ = std::fs::remove_dir_all(cwd);
    }
}
