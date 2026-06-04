use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingEffort {
    None,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
}

impl fmt::Display for ThinkingEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            ThinkingEffort::None => "none",
            ThinkingEffort::Low => "low",
            ThinkingEffort::Medium => "medium",
            ThinkingEffort::High => "high",
            ThinkingEffort::XHigh => "xhigh",
            ThinkingEffort::Max => "max",
        };
        f.write_str(value)
    }
}

impl FromStr for ThinkingEffort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(ThinkingEffort::None),
            "low" => Ok(ThinkingEffort::Low),
            "medium" => Ok(ThinkingEffort::Medium),
            "high" => Ok(ThinkingEffort::High),
            "xhigh" => Ok(ThinkingEffort::XHigh),
            "max" => Ok(ThinkingEffort::Max),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Text,
    Image,
}

impl fmt::Display for InputModality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            InputModality::Text => "text",
            InputModality::Image => "image",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProviderEndpointKind {
    OpenAI,
    Anthropic,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub limit: u32,
    pub thinking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<InputModality>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub endpoint: ProviderEndpointKind,
    pub base_url: String,
    pub models: Vec<ModelInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_info_input_modalities_can_be_undeclared() {
        let model: ModelInfo = serde_json::from_value(json!({
            "id": "gpt-test",
            "name": null,
            "limit": 256000,
            "thinking": false
        }))
        .unwrap();

        assert_eq!(model.input_modalities, None);
    }

    #[test]
    fn model_info_input_modalities_parse_image() {
        let model: ModelInfo = serde_json::from_value(json!({
            "id": "gpt-test",
            "name": null,
            "limit": 256000,
            "thinking": false,
            "input_modalities": ["text", "image"]
        }))
        .unwrap();

        assert_eq!(
            model.input_modalities.as_deref(),
            Some(&[InputModality::Text, InputModality::Image][..])
        );
    }

    #[test]
    fn thinking_effort_parses_new_levels() {
        assert_eq!("xhigh".parse(), Ok(ThinkingEffort::XHigh));
        assert_eq!("max".parse(), Ok(ThinkingEffort::Max));
    }

    #[test]
    fn thinking_effort_serializes_new_levels() {
        assert_eq!(
            serde_json::to_value(ThinkingEffort::XHigh).unwrap(),
            json!("xhigh")
        );
        assert_eq!(
            serde_json::to_value(ThinkingEffort::Max).unwrap(),
            json!("max")
        );
    }
}
