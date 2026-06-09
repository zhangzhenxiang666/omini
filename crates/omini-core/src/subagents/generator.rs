use super::GeneratedAgentDraft;
use crate::types::config::{Settings, ThinkingEffort};
use omini_provider_api::{ApiEvent, ApiRequest, LlmClient};
use std::fmt;
use tokio_stream::StreamExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerateAgentDraftError {
    Request(String),
    Stream(String),
    Parse(String),
}

impl fmt::Display for GenerateAgentDraftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenerateAgentDraftError::Request(message)
            | GenerateAgentDraftError::Stream(message)
            | GenerateAgentDraftError::Parse(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GenerateAgentDraftError {}

pub async fn generate_agent_draft_checked_from_settings(
    settings: &Settings,
    description: &str,
) -> Result<GeneratedAgentDraft, GenerateAgentDraftError> {
    let llm_client = LlmClient::new(
        settings.endpoint,
        settings.api_key.clone(),
        settings.base_url.clone(),
    );
    generate_agent_draft_checked(
        &llm_client,
        &settings.model,
        settings.thinking_effort,
        description,
    )
    .await
}

async fn generate_agent_draft_checked(
    llm_client: &LlmClient,
    model: &str,
    thinking_effort: Option<ThinkingEffort>,
    description: &str,
) -> Result<GeneratedAgentDraft, GenerateAgentDraftError> {
    let prompt = build_generate_agent_prompt(description);
    let messages = vec![crate::types::message::Message {
        role: crate::types::message::Role::User,
        content: vec![crate::types::message::ContentBlock::from_text(prompt)],
    }];
    let request = ApiRequest {
        messages: &messages,
        model,
        system_prompt: Some(
            "You generate Omini subagent specs. Output only valid JSON matching the requested schema.",
        ),
        tools: None,
        max_tokens: Some(8192),
        temperature: Some(0.2),
        thinking_effort,
    };
    let mut stream = llm_client
        .invoke(request)
        .await
        .map_err(|e| GenerateAgentDraftError::Request(format!("生成 agent 失败: {e}")))?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(ApiEvent::Text(delta)) => text.push_str(&delta),
            Ok(ApiEvent::Done(_)) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(GenerateAgentDraftError::Stream(format!(
                    "生成 agent 流式响应失败: {e}"
                )));
            }
        }
    }
    parse_generated_agent(&text).map_err(GenerateAgentDraftError::Parse)
}

fn build_generate_agent_prompt(description: &str) -> String {
    format!(
        "Generate an Omini subagent definition from the user's description.\n\
         Return only one valid JSON object, with no Markdown fences, comments, or commentary.\n\n\
         Required JSON fields:\n\
         - name: short kebab-case English identifier, specific to this agent's job.\n\
         - description: one concise sentence explaining when the parent agent should use this subagent.\n\
         - instructions: complete system instructions for the subagent.\n\n\
         A compliant Omini subagent is specialized, bounded, and directly useful to a parent agent.\n\
         The instructions must:\n\
         - Define the subagent's exact responsibility and scope.\n\
         - Tell it to inspect enough context before answering or editing.\n\
         - Tell it to keep unrelated files and behavior untouched.\n\
         - Define the expected final response shape for the parent agent, including evidence, changed files, verification, or uncertainty when relevant.\n\
         - Avoid assuming specific tools are available; tool policy is configured outside this generated JSON.\n\
         - Forbid spawning, delegating to, creating, or asking for another subagent.\n\n\
         The generated subagent must not mention a subagent tool, delegation tool, nested agents, or parallel agents.\n\
         Do not include extra JSON fields.\n\n\
         User description:\n{}",
        description.trim()
    )
}

pub fn parse_generated_agent(raw: &str) -> Result<GeneratedAgentDraft, String> {
    let mut json_text = raw.trim();
    if let Some(stripped) = json_text.strip_prefix("```json") {
        json_text = stripped.trim();
    } else if let Some(stripped) = json_text.strip_prefix("```") {
        json_text = stripped.trim();
    }
    if let Some(stripped) = json_text.strip_suffix("```") {
        json_text = stripped.trim();
    }
    let value: serde_json::Value =
        serde_json::from_str(json_text).map_err(|e| format!("解析生成结果失败: {e}"))?;
    let field = |key: &str| -> Result<String, String> {
        value
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("生成结果缺少字段: {key}"))
    };
    Ok(GeneratedAgentDraft {
        name: field("name")?,
        description: field("description")?,
        instructions: field("instructions")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_generated_agent_accepts_plain_json() {
        let draft = parse_generated_agent(
            r#"{
                "name": "diff-reviewer",
                "description": "Use when reviewing focused code diffs.",
                "instructions": "Review the diff and return findings."
            }"#,
        )
        .unwrap();

        assert_eq!(draft.name, "diff-reviewer");
        assert_eq!(draft.description, "Use when reviewing focused code diffs.");
        assert_eq!(draft.instructions, "Review the diff and return findings.");
    }

    #[test]
    fn parse_generated_agent_accepts_fenced_json() {
        let draft = parse_generated_agent(
            r#"```json
            {
                "name": "test-writer",
                "description": "Use when adding focused tests.",
                "instructions": "Add tests and report verification."
            }
            ```"#,
        )
        .unwrap();

        assert_eq!(draft.name, "test-writer");
    }

    #[test]
    fn parse_generated_agent_rejects_missing_required_field() {
        let err = parse_generated_agent(
            r#"{
                "name": "bad",
                "description": "Missing instructions."
            }"#,
        )
        .unwrap_err();

        assert!(err.contains("生成结果缺少字段: instructions"));
    }

    #[test]
    fn generation_prompt_defines_compliant_subagent_constraints() {
        let prompt = build_generate_agent_prompt("review diffs");

        assert!(prompt.contains("specialized, bounded"));
        assert!(prompt.contains("when the parent agent should use this subagent"));
        assert!(prompt.contains("tool policy is configured outside"));
        assert!(prompt.contains("Forbid spawning"));
        assert!(prompt.contains("Do not include extra JSON fields"));
        assert!(prompt.contains("review diffs"));
    }
}
