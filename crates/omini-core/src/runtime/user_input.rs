use crate::CoreError;
use crate::runtime::CapabilityStore;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use omini_config::Settings;
use omini_domain::config::InputModality;
use omini_domain::input::{InputPart, RunCommand};
use omini_model::message::{ContentBlock, Message, Role};
use omini_runtime_contract::thread::RuntimeUserInput;
use sha2::{Digest, Sha256};

pub const INIT_PROMPT: &str = include_str!("../prompts/init.txt");

/// 校验并构建用户输入的 LLM 上下文消息：skill 展开、init prompt 注入、
/// 附件完整性校验与 base64 编码都是 core 知识，因此留在这里。
/// 用户时间线记录与此无关，由 server 在接收路径构建。
pub fn prepare_submission(
    input: RuntimeUserInput,
    command: Option<RunCommand>,
    settings: &Settings,
    capabilities: &CapabilityStore,
) -> Result<Message, CoreError> {
    validate_non_empty(&input, command)?;
    if !input.attachments.is_empty() && !settings.supports_input_modality(InputModality::Image) {
        return Err(CoreError::invalid_input(
            "unsupported_input_modality",
            format!(
                "model '{}/{}' does not support image input",
                settings.active_model().provider_id,
                settings.active_model().model_id
            ),
        ));
    }

    let mut content = Vec::new();
    if command == Some(RunCommand::Init) {
        content.push(ContentBlock::from_text(INIT_PROMPT.trim().to_string()));
    }

    let skills = capabilities.skill_registry();
    let subagents = capabilities.subagent_registry();
    for part in &input.parts {
        let text = match part {
            InputPart::Text { text } => {
                if text.is_empty() {
                    return Err(CoreError::invalid_input(
                        "invalid_input_part",
                        "text parts must not be empty",
                    ));
                }
                text.clone()
            }
            InputPart::Skill { name } => {
                let skill = skills.get(name).ok_or_else(|| {
                    CoreError::invalid_input(
                        "skill_not_found",
                        format!("skill '{name}' does not exist"),
                    )
                })?;
                if !skill.user_invocable {
                    return Err(CoreError::invalid_input(
                        "skill_not_invocable",
                        format!("skill '{name}' cannot be invoked by the user"),
                    ));
                }
                crate::skills::render_skill_invocation(skill, None)
            }
            InputPart::File { path, .. } => {
                format!("File: {path}. Read this file if needed.")
            }
            InputPart::Directory { path, .. } => {
                format!("Directory: {path}. Inspect this directory if needed.")
            }
            InputPart::Subagent { name, .. } => {
                let agent = subagents.get(name).ok_or_else(|| {
                    CoreError::invalid_input(
                        "subagent_not_found",
                        format!("subagent '{name}' does not exist"),
                    )
                })?;
                format!(
                    "Agent: @{name} ({}). Use subagent \"{name}\" if this helps answer the user.",
                    agent.description
                )
            }
        };
        content.push(ContentBlock::from_text(text));
    }

    let mut attachments = input.attachments;
    attachments.sort_by(|left, right| {
        left.metadata
            .attachment_id
            .cmp(&right.metadata.attachment_id)
    });
    for attachment in &attachments {
        let bytes = std::fs::read(&attachment.source_path).map_err(|error| {
            CoreError::invalid_input(
                "attachment_not_found",
                format!(
                    "failed to read attachment '{}': {error}",
                    attachment.metadata.attachment_id
                ),
            )
        })?;
        let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
        if bytes.len() as u64 != attachment.metadata.size || actual_sha256 != attachment.sha256 {
            return Err(CoreError::invalid_input(
                "attachment_not_found",
                format!(
                    "attachment '{}' failed its integrity check",
                    attachment.metadata.attachment_id
                ),
            ));
        }
        content.push(ContentBlock::from_base64_image(
            attachment.metadata.mime_type.clone(),
            BASE64_STANDARD.encode(bytes),
        ));
    }

    Ok(Message::new(Role::User, content))
}

fn validate_non_empty(
    input: &RuntimeUserInput,
    command: Option<RunCommand>,
) -> Result<(), CoreError> {
    let meaningful_part = input.parts.iter().any(|part| match part {
        InputPart::Text { text } => !text.trim().is_empty(),
        _ => true,
    });
    if command.is_none() && !meaningful_part && input.attachments.is_empty() {
        return Err(CoreError::invalid_input(
            "invalid_input_part",
            "message input must contain a meaningful part or attachment",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestTempDir;
    use omini_domain::input::AttachmentMetadata;
    use omini_runtime_contract::thread::ResolvedAttachment;

    #[test]
    fn expands_parts_in_their_original_order() {
        let root = TestTempDir::new("typed-input-order");
        root.write(
            ".omini/skills/ordered/SKILL.md",
            "---\nname: ordered\ndescription: Ordered test\n---\nSKILL BODY\n",
        );
        let settings = crate::test_support::settings(root.path(), false);
        let capabilities = CapabilityStore::load(&settings);
        let parts = vec![
            InputPart::Text {
                text: "before ".to_string(),
            },
            InputPart::Skill {
                name: "ordered".to_string(),
            },
            InputPart::File {
                path: "src/lib.rs".to_string(),
                label: Some("same label".to_string()),
            },
            InputPart::Text {
                text: " after".to_string(),
            },
        ];

        let message = prepare_submission(
            RuntimeUserInput {
                parts,
                attachments: Vec::new(),
            },
            None,
            &settings,
            &capabilities,
        )
        .expect("input should prepare");

        let texts = message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text(block) => block.text.as_str(),
                _ => panic!("expected text block"),
            })
            .collect::<Vec<_>>();
        assert_eq!(texts[0], "before ");
        assert!(texts[1].contains("SKILL BODY"));
        assert_eq!(texts[2], "File: src/lib.rs. Read this file if needed.");
        assert_eq!(texts[3], " after");
    }

    #[test]
    fn rejects_skills_that_are_not_user_invocable() {
        let root = TestTempDir::new("typed-input-private-skill");
        root.write(
            ".omini/skills/private/SKILL.md",
            "---\nname: private\ndescription: Private test\nuser-invocable: false\n---\nPRIVATE BODY\n",
        );
        let settings = crate::test_support::settings(root.path(), false);
        let capabilities = CapabilityStore::load(&settings);
        let error = prepare_submission(
            RuntimeUserInput {
                parts: vec![InputPart::Skill {
                    name: "private".to_string(),
                }],
                attachments: Vec::new(),
            },
            None,
            &settings,
            &capabilities,
        )
        .expect_err("private skill must not be user invocable");

        assert_eq!(error.code(), "skill_not_invocable");
    }

    #[test]
    fn sorts_images_by_opaque_id_and_rejects_non_image_models() {
        let root = TestTempDir::new("typed-input-images");
        let first_path = root.write("first.png", b"first");
        let second_path = root.write("second.png", b"second");
        let attachment = |id: &str, path: std::path::PathBuf, bytes: &[u8]| ResolvedAttachment {
            metadata: AttachmentMetadata {
                attachment_id: id.to_string(),
                mime_type: "image/png".to_string(),
                size: bytes.len() as u64,
                name: format!("{id}.png"),
            },
            sha256: format!("{:x}", Sha256::digest(bytes)),
            source_path: path,
        };
        let input = RuntimeUserInput {
            parts: Vec::new(),
            attachments: vec![
                attachment("b", second_path, b"second"),
                attachment("a", first_path, b"first"),
            ],
        };

        let text_settings = crate::test_support::settings(root.path(), false);
        let text_capabilities = CapabilityStore::load(&text_settings);
        let error = prepare_submission(input.clone(), None, &text_settings, &text_capabilities)
            .expect_err("text-only model must reject images");
        assert_eq!(error.code(), "unsupported_input_modality");

        let vision_settings = crate::test_support::settings(root.path(), true);
        let vision_capabilities = CapabilityStore::load(&vision_settings);
        let message = prepare_submission(input, None, &vision_settings, &vision_capabilities)
            .expect("vision model should accept images");
        assert!(message.content.iter().all(ContentBlock::is_image));

        let corrupt = RuntimeUserInput {
            parts: Vec::new(),
            attachments: vec![ResolvedAttachment {
                metadata: AttachmentMetadata {
                    attachment_id: "corrupt".to_string(),
                    mime_type: "image/png".to_string(),
                    size: 5,
                    name: "corrupt.png".to_string(),
                },
                sha256: "0".repeat(64),
                source_path: root.path().join("first.png"),
            }],
        };
        let error = prepare_submission(corrupt, None, &vision_settings, &vision_capabilities)
            .expect_err("attachment hash mismatch must fail");
        assert_eq!(error.code(), "attachment_not_found");
    }

    #[test]
    fn init_prompt_is_core_owned() {
        let root = TestTempDir::new("typed-input-init");
        let settings = crate::test_support::settings(root.path(), false);
        let capabilities = CapabilityStore::load(&settings);
        let message = prepare_submission(
            RuntimeUserInput {
                parts: Vec::new(),
                attachments: Vec::new(),
            },
            Some(RunCommand::Init),
            &settings,
            &capabilities,
        )
        .expect("empty init command should prepare");

        assert!(matches!(message.content[0], ContentBlock::Text(_)));
    }
}
