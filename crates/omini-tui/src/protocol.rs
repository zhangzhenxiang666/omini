use crate::display as display_types;
use crate::types::config as config_types;
use crate::types::events as event_types;
use omini_domain::subagents as subagent_types;

#[derive(Debug, Clone)]
pub(crate) struct ClientUserInput {
    pub input: omini_protocol::UserInput,
    pub images: Vec<display_types::DisplayImageAttachment>,
}

pub(crate) fn user_input_from_draft(draft: display_types::UserDraft) -> ClientUserInput {
    enum Atom {
        Mention(display_types::DisplayMention),
        Image(display_types::DisplayImageAttachment),
    }

    impl Atom {
        fn span(&self) -> (usize, usize) {
            match self {
                Self::Mention(value) => (value.start_char, value.end_char),
                Self::Image(value) => (value.start_char, value.end_char),
            }
        }
    }

    let mut atoms = draft
        .mentions
        .into_iter()
        .filter(|mention| mention.kind != display_types::MentionKind::Command)
        .map(Atom::Mention)
        .chain(draft.images.iter().cloned().map(Atom::Image))
        .collect::<Vec<_>>();
    atoms.sort_by_key(Atom::span);

    let chars = draft.text.chars().collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut cursor = 0usize;
    for atom in atoms {
        let (start, end) = atom.span();
        if start > cursor {
            push_text_part(&mut parts, chars[cursor..start].iter().collect());
        }
        match atom {
            Atom::Mention(mention) => parts.push(input_part_from_mention(mention)),
            Atom::Image(_) => {}
        }
        cursor = end.max(cursor);
    }
    if cursor < chars.len() {
        push_text_part(&mut parts, chars[cursor..].iter().collect());
    }

    ClientUserInput {
        input: omini_protocol::UserInput {
            parts,
            attachment_ids: Vec::new(),
        },
        images: draft.images,
    }
}

pub(crate) fn command_input_from_draft(
    draft: display_types::UserDraft,
    command_name: &str,
) -> ClientUserInput {
    user_input_from_draft(strip_slash_command(draft, command_name))
}

pub(crate) fn skill_input_from_draft(
    draft: display_types::UserDraft,
    skill_name: String,
) -> ClientUserInput {
    let mut input = command_input_from_draft(draft, &skill_name);
    input
        .input
        .parts
        .insert(0, omini_protocol::InputPart::Skill { name: skill_name });
    input
}

fn strip_slash_command(
    mut draft: display_types::UserDraft,
    command_name: &str,
) -> display_types::UserDraft {
    let prefix_len = command_name.chars().count() + 1;
    let chars = draft.text.chars().collect::<Vec<_>>();
    if chars.len() < prefix_len {
        return draft;
    }
    draft.text = chars[prefix_len..].iter().collect();
    draft.mentions = draft
        .mentions
        .into_iter()
        .filter_map(|mut mention| {
            (mention.start_char >= prefix_len).then(|| {
                mention.start_char -= prefix_len;
                mention.end_char -= prefix_len;
                mention
            })
        })
        .collect();
    draft.images = draft
        .images
        .into_iter()
        .filter_map(|mut image| {
            (image.start_char >= prefix_len).then(|| {
                image.start_char -= prefix_len;
                image.end_char -= prefix_len;
                image
            })
        })
        .collect();
    draft
}

fn push_text_part(parts: &mut Vec<omini_protocol::InputPart>, text: String) {
    if !text.is_empty() {
        parts.push(omini_protocol::InputPart::Text { text });
    }
}

fn input_part_from_mention(mention: display_types::DisplayMention) -> omini_protocol::InputPart {
    match mention.kind {
        display_types::MentionKind::File => omini_protocol::InputPart::File {
            path: mention.target,
            label: Some(mention.label),
        },
        display_types::MentionKind::Directory => omini_protocol::InputPart::Directory {
            path: mention.target,
            label: Some(mention.label),
        },
        display_types::MentionKind::Subagent => omini_protocol::InputPart::Subagent {
            name: mention.target,
            label: Some(mention.label),
        },
        display_types::MentionKind::Command => unreachable!("command mentions are filtered"),
    }
}

pub(crate) fn thinking_effort_from_internal(
    effort: config_types::ThinkingEffort,
) -> omini_protocol::ThinkingEffort {
    match effort {
        config_types::ThinkingEffort::None => omini_protocol::ThinkingEffort::None,
        config_types::ThinkingEffort::Low => omini_protocol::ThinkingEffort::Low,
        config_types::ThinkingEffort::Medium => omini_protocol::ThinkingEffort::Medium,
        config_types::ThinkingEffort::High => omini_protocol::ThinkingEffort::High,
        config_types::ThinkingEffort::XHigh => omini_protocol::ThinkingEffort::XHigh,
        config_types::ThinkingEffort::Max => omini_protocol::ThinkingEffort::Max,
    }
}

pub(crate) fn active_profile_from_internal(
    profile: event_types::ActiveProfile,
) -> omini_protocol::ActiveProfile {
    match profile {
        event_types::ActiveProfile::Main => omini_protocol::ActiveProfile::Main,
        event_types::ActiveProfile::Auto => omini_protocol::ActiveProfile::Auto,
        event_types::ActiveProfile::Plan => omini_protocol::ActiveProfile::Plan,
    }
}

pub(crate) fn tool_pause_response_from_internal(
    response: event_types::ToolPauseResponse,
) -> omini_protocol::ToolPauseResponse {
    match response {
        event_types::ToolPauseResponse::Permission { approved, note } => {
            omini_protocol::ToolPauseResponse::Permission { approved, note }
        }
        event_types::ToolPauseResponse::UserInput { value } => {
            omini_protocol::ToolPauseResponse::UserInput { value }
        }
        event_types::ToolPauseResponse::Cancelled => omini_protocol::ToolPauseResponse::Cancelled,
    }
}

pub(crate) fn plan_approval_action_from_internal(
    action: event_types::PlanApprovalAction,
) -> omini_protocol::PlanApprovalAction {
    match action {
        event_types::PlanApprovalAction::Approve { profile } => {
            omini_protocol::PlanApprovalAction::Approve {
                profile: plan_execution_profile_from_internal(profile),
            }
        }
        event_types::PlanApprovalAction::ApproveInNewThread { profile } => {
            omini_protocol::PlanApprovalAction::ApproveInNewThread {
                profile: plan_execution_profile_from_internal(profile),
            }
        }
        event_types::PlanApprovalAction::ContinueDiscussing => {
            omini_protocol::PlanApprovalAction::ContinueDiscussing
        }
    }
}

fn plan_execution_profile_from_internal(
    profile: event_types::PlanExecutionProfile,
) -> omini_protocol::PlanExecutionProfile {
    match profile {
        event_types::PlanExecutionProfile::Main => omini_protocol::PlanExecutionProfile::Main,
        event_types::PlanExecutionProfile::Auto => omini_protocol::PlanExecutionProfile::Auto,
    }
}

pub(crate) fn agent_source_kind_from_internal(
    source_kind: subagent_types::AgentSourceKind,
) -> omini_protocol::AgentSourceKind {
    match source_kind {
        subagent_types::AgentSourceKind::BuiltIn => omini_protocol::AgentSourceKind::BuiltIn,
        subagent_types::AgentSourceKind::Project => omini_protocol::AgentSourceKind::Project,
        subagent_types::AgentSourceKind::User => omini_protocol::AgentSourceKind::User,
    }
}

pub(crate) fn agent_draft_from_internal(
    draft: subagent_types::AgentDraft,
) -> omini_protocol::AgentDraft {
    omini_protocol::AgentDraft {
        name: draft.name,
        description: draft.description,
        short_description: draft.short_description,
        instructions: draft.instructions,
        tools: draft.tools,
        disallow_tools: draft.disallow_tools,
        model: draft.model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn char_index(text: &str, needle: &str) -> usize {
        text[..text.find(needle).expect("needle should exist")]
            .chars()
            .count()
    }

    #[test]
    fn draft_atoms_preserve_unicode_order_labels_and_whitespace() {
        let text = "甲@same 乙[Image 1] 丙@same".to_string();
        let first_start = char_index(&text, "@same");
        let image_start = char_index(&text, "[Image 1]");
        let second_start = text
            .char_indices()
            .filter_map(|(index, _)| text[index..].starts_with("@same").then_some(index))
            .nth(1)
            .map(|byte| text[..byte].chars().count())
            .expect("second mention should exist");
        let input = user_input_from_draft(display_types::UserDraft {
            text,
            mentions: vec![
                display_types::DisplayMention {
                    start_char: first_start,
                    end_char: first_start + "@same".chars().count(),
                    kind: display_types::MentionKind::File,
                    label: "same".to_string(),
                    target: "first.txt".to_string(),
                    description: String::new(),
                },
                display_types::DisplayMention {
                    start_char: second_start,
                    end_char: second_start + "@same".chars().count(),
                    kind: display_types::MentionKind::Directory,
                    label: "same".to_string(),
                    target: "second".to_string(),
                    description: String::new(),
                },
            ],
            images: vec![display_types::DisplayImageAttachment {
                start_char: image_start,
                end_char: image_start + "[Image 1]".chars().count(),
                marker: "[Image 1]".to_string(),
                source_path: "/tmp/image.png".to_string(),
                file_name: "image.png".to_string(),
            }],
        });

        assert_eq!(
            input.input.parts,
            vec![
                omini_protocol::InputPart::Text {
                    text: "甲".to_string(),
                },
                omini_protocol::InputPart::File {
                    path: "first.txt".to_string(),
                    label: Some("same".to_string()),
                },
                omini_protocol::InputPart::Text {
                    text: " 乙".to_string(),
                },
                omini_protocol::InputPart::Text {
                    text: " 丙".to_string(),
                },
                omini_protocol::InputPart::Directory {
                    path: "second".to_string(),
                    label: Some("same".to_string()),
                },
            ]
        );
        assert_eq!(input.images.len(), 1);
        assert!(input.input.attachment_ids.is_empty());
    }

    #[test]
    fn skill_part_precedes_unmodified_argument_parts() {
        let input = skill_input_from_draft(
            display_types::UserDraft::plain("/review  原始参数".to_string()),
            "review".to_string(),
        );

        assert_eq!(
            input.input.parts,
            vec![
                omini_protocol::InputPart::Skill {
                    name: "review".to_string(),
                },
                omini_protocol::InputPart::Text {
                    text: "  原始参数".to_string(),
                },
            ]
        );
    }
}
