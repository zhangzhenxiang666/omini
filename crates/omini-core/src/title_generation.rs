use omini_config::{ModelTier, Settings};
use omini_domain::message::{ContentBlock, Message, Role};
use omini_provider_api::{ApiEvent, ApiRequest, LlmClient};
use std::fmt;
use tokio_stream::StreamExt;

const TITLE_MAX_CHARS: usize = 300;
const TITLE_MAX_TOKENS: u64 = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleGenError {
    Request(String),
    Stream(String),
    Parse(String),
}

impl fmt::Display for TitleGenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TitleGenError::Request(message)
            | TitleGenError::Stream(message)
            | TitleGenError::Parse(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for TitleGenError {}

/// 后台异步生成 thread 标题。`Err(_)` 表示请求 / 流 / JSON 解析失败，
/// 调用方统一按 "保留兜底 title, 记 tracing::warn" 处理。
pub async fn generate_thread_title(
    settings: &Settings,
    user_input: &str,
) -> Result<String, TitleGenError> {
    let (provider_key, model, thinking_effort) = settings.resolve_tier(ModelTier::Small);
    let profile = settings.providers.get(&provider_key).ok_or_else(|| {
        TitleGenError::Request(format!(
            "tier provider {provider_key} missing after resolve"
        ))
    })?;
    let model_config = profile
        .models
        .iter()
        .find(|candidate| candidate.id == model)
        .ok_or_else(|| {
            TitleGenError::Request(format!("tier model {model} missing after resolve"))
        })?;
    let llm_client = LlmClient::new(
        profile.endpoint,
        profile.api_key.clone(),
        profile.base_url.clone(),
    );
    let prompt = build_generate_title_prompt(user_input);
    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::from_text(prompt)],
    }];
    let request = ApiRequest {
        messages: &messages,
        model: &model,
        system_prompt: Some(
            "You are a thread title generator. \
             You output ONLY a valid JSON object with one \"title\" field. Nothing else.",
        ),
        tools: None,
        max_tokens: Some(TITLE_MAX_TOKENS),
        temperature: Some(0.2),
        thinking_effort,
        extra_headers: model_config.extra_headers.as_ref(),
        extra_body: model_config.extra_body.as_ref(),
    };
    let mut stream = llm_client
        .invoke(request)
        .await
        .map_err(|e| TitleGenError::Request(format!("generate thread title failed: {e}")))?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(ApiEvent::Text(delta)) => text.push_str(&delta),
            Ok(ApiEvent::Done(_)) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(TitleGenError::Stream(format!(
                    "generate thread title stream failed: {e}"
                )));
            }
        }
    }
    let parsed = parse_generated_title(&text)?;
    Ok(truncate_to_max_chars(&parsed, TITLE_MAX_CHARS))
}

fn build_generate_title_prompt(user_input: &str) -> String {
    format!(
        "Generate a brief title that would help the user find this conversation later.\n\
         The title is shown in thread history lists and the start screen.\n\
         \n\
         <rules>\n\
         - Use the same language as the user message you are summarizing\n\
         - Title must be ≤50 characters, grammatically correct, and read naturally - no word salad\n\
         - Never include tool names in the title (e.g. \"read tool\", \"bash tool\", \"edit tool\")\n\
         - Focus on the main topic or question the user needs to retrieve\n\
         - Vary your phrasing - avoid repetitive patterns like always starting with \"Analyzing\"\n\
         - When a file is mentioned, focus on WHAT the user wants to do WITH the file\n\
         - Keep exact: technical terms, numbers, filenames, HTTP codes\n\
         - Remove: the, this, my, a, an\n\
         - Never respond to questions, just generate a title for the conversation\n\
         - The title should NEVER include \"summarizing\" or \"generating\" when generating a title\n\
         - Always output something meaningful, even if the input is minimal\n\
         - If the user message is short or conversational (e.g. \"hello\", \"lol\", \"what's up\", \"hey\"):\n\
         -> create a title that reflects the user's tone or intent (such as Greeting, Quick check-in, etc.)\n\
         </rules>\n\
         \n\
         Return only one valid JSON object with this exact shape and no other fields:\n\
         {{\n\
         \x20\x20\"title\": \"<title in the user's language>\"\n\
         }}\n\
         \n\
         <examples>\n\
         \"debug 500 errors in production\" -> {{\"title\": \"Debugging production 500 errors\"}}\n\
         \"refactor user service\" -> {{\"title\": \"Refactoring user service\"}}\n\
         \"why is app.js failing\" -> {{\"title\": \"app.js failure investigation\"}}\n\
         </examples>\n\
         \n\
         First user message:\n\
         {user_input}"
    )
}

/// 严格解析 LLM 输出为 JSON，提取 `title` 字段，校验非空。
/// 与 `subagents::generator::parse_generated_agent` 完全对称：
/// 支持裸 JSON，也支持 ```json / ``` 围栏。
fn parse_generated_title(raw: &str) -> Result<String, TitleGenError> {
    let mut json_text = raw.trim();
    if let Some(stripped) = json_text.strip_prefix("```json") {
        json_text = stripped.trim();
    } else if let Some(stripped) = json_text.strip_prefix("```") {
        json_text = stripped.trim();
    }
    if let Some(stripped) = json_text.strip_suffix("```") {
        json_text = stripped.trim();
    }
    let value: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|e| TitleGenError::Parse(format!("parse generated title json failed: {e}")))?;
    let title = value
        .get("title")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            TitleGenError::Parse("generated title missing or empty `title` field".to_string())
        })?;
    Ok(title.to_owned())
}

fn truncate_to_max_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_to_max_chars_keeps_codepoint_boundary() {
        let long = "啊".repeat(400);
        assert_eq!(
            truncate_to_max_chars(&long, TITLE_MAX_CHARS)
                .chars()
                .count(),
            TITLE_MAX_CHARS
        );
    }

    #[test]
    fn generated_title_json_and_fences_return_trimmed_title() {
        let cases = [
            (r#"{"title":"Fix login bug"}"#, "Fix login bug"),
            (
                "```json\n{\"title\": \"修复登录 bug\"}\n```",
                "修复登录 bug",
            ),
            (
                "```\n{\"title\": \"  Review flaky test  \"}\n```",
                "Review flaky test",
            ),
        ];

        for (raw, expected) in cases {
            assert_eq!(parse_generated_title(raw), Ok(expected.into()));
        }
    }

    #[test]
    fn generated_title_invalid_shape_or_json_returns_parse_error() {
        for raw in [
            r#"{"description":"missing"}"#,
            r#"{"title":"   "}"#,
            "not json",
        ] {
            assert!(matches!(
                parse_generated_title(raw),
                Err(TitleGenError::Parse(_))
            ));
        }
    }
}
