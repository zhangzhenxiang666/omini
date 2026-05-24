use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::skills::SkillSpec;
use crate::types::display::{
    DisplayMention, DisplayMessage, MentionKind, UserDraft, referenced_context_text,
};
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

fn build_skill_query(
    spec: &SkillSpec,
    args: &str,
    draft: Option<&UserDraft>,
) -> (Message, DisplayMessage) {
    let prompt = args.trim();
    let command_text = format!("/{}", spec.name);
    let mut llm_text = crate::skills::render_skill_slash_command_invocation(spec, Some(prompt));
    if let Some(context) = draft.and_then(|draft| referenced_context_text(&draft.mentions)) {
        llm_text.push_str("\n\n");
        llm_text.push_str(&context);
    }
    let display_text = draft.map(|draft| draft.text.clone()).unwrap_or_else(|| {
        if prompt.is_empty() {
            command_text.clone()
        } else {
            format!("{command_text} {prompt}")
        }
    });

    let command_end = command_text.chars().count();
    let mut mentions = vec![DisplayMention {
        start_char: 0,
        end_char: command_end,
        kind: MentionKind::Command,
        label: spec.name.clone(),
        target: spec.name.clone(),
        description: spec.description.clone(),
    }];
    if let Some(draft) = draft {
        mentions.extend(draft.mentions.iter().cloned());
        mentions.sort_by_key(|mention| mention.start_char);
    }
    (
        Message::from_user_text(llm_text),
        DisplayMessage {
            role: Role::User,
            text: display_text,
            mentions,
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

    async fn execute(
        &self,
        _runtime: &mut AgentRuntime,
        args: &str,
        draft: &UserDraft,
    ) -> CommandResult {
        let (llm_message, display_message) = build_skill_query(&self.spec, args, Some(draft));
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
        let (llm_message, display) = build_skill_query(spec, "split the staged diff", None);

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

    #[test]
    fn skill_display_and_llm_text_include_argument_mentions() {
        let cwd =
            std::env::temp_dir().join(format!("omini-skill-mention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let registry = crate::skills::load_skill_registry(&cwd);
        let spec = registry.get("commit-message").unwrap();
        let draft = UserDraft {
            text: "/commit-message summarize @src/main.rs".to_string(),
            mentions: vec![DisplayMention {
                start_char: 26,
                end_char: 38,
                kind: MentionKind::File,
                label: "src/main.rs".to_string(),
                target: "src/main.rs".to_string(),
                description: "file".to_string(),
            }],
            images: Vec::new(),
        };

        let (llm_message, display) =
            build_skill_query(spec, "summarize @src/main.rs", Some(&draft));

        assert_eq!(display.text, "/commit-message summarize @src/main.rs");
        assert_eq!(display.mentions.len(), 2);
        assert_eq!(display.mentions[0].kind, MentionKind::Command);
        assert_eq!(display.mentions[1].kind, MentionKind::File);
        let ContentBlock::Text(text) = &llm_message.content[0] else {
            panic!("skill command should inject a text block");
        };
        assert!(text.text.contains("<skill_invocation>"));
        assert!(text.text.contains("Referenced context:"));
        assert!(
            text.text
                .contains("File: src/main.rs. Read this file if needed.")
        );
        let _ = std::fs::remove_dir_all(cwd);
    }
}
