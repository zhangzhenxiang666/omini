//! 模型目录和选择值的协议类型。

use super::*;
use serde_json::Value;
use std::collections::HashMap;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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

impl std::fmt::Display for ThinkingEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        })
    }
}

impl std::str::FromStr for ThinkingEffort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProviderEndpointKind {
    OpenAI,
    Anthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Text,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub limit: u32,
    pub thinking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<InputModality>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub endpoint: ProviderEndpointKind,
    pub base_url: String,
    pub models: Vec<ModelInfo>,
}

impl From<ThinkingEffort> for omini_domain::config::ThinkingEffort {
    fn from(effort: ThinkingEffort) -> Self {
        match effort {
            ThinkingEffort::None => Self::None,
            ThinkingEffort::Low => Self::Low,
            ThinkingEffort::Medium => Self::Medium,
            ThinkingEffort::High => Self::High,
            ThinkingEffort::XHigh => Self::XHigh,
            ThinkingEffort::Max => Self::Max,
        }
    }
}

impl From<omini_domain::config::ThinkingEffort> for ThinkingEffort {
    fn from(effort: omini_domain::config::ThinkingEffort) -> Self {
        match effort {
            omini_domain::config::ThinkingEffort::None => Self::None,
            omini_domain::config::ThinkingEffort::Low => Self::Low,
            omini_domain::config::ThinkingEffort::Medium => Self::Medium,
            omini_domain::config::ThinkingEffort::High => Self::High,
            omini_domain::config::ThinkingEffort::XHigh => Self::XHigh,
            omini_domain::config::ThinkingEffort::Max => Self::Max,
        }
    }
}

impl From<ProviderEndpointKind> for omini_domain::config::ProviderEndpointKind {
    fn from(endpoint: ProviderEndpointKind) -> Self {
        match endpoint {
            ProviderEndpointKind::OpenAI => Self::OpenAI,
            ProviderEndpointKind::Anthropic => Self::Anthropic,
        }
    }
}

impl From<omini_domain::config::ProviderEndpointKind> for ProviderEndpointKind {
    fn from(endpoint: omini_domain::config::ProviderEndpointKind) -> Self {
        match endpoint {
            omini_domain::config::ProviderEndpointKind::OpenAI => Self::OpenAI,
            omini_domain::config::ProviderEndpointKind::Anthropic => Self::Anthropic,
        }
    }
}

impl From<omini_domain::config::InputModality> for InputModality {
    fn from(modality: omini_domain::config::InputModality) -> Self {
        match modality {
            omini_domain::config::InputModality::Text => Self::Text,
            omini_domain::config::InputModality::Image => Self::Image,
        }
    }
}

impl From<omini_domain::config::ModelInfo> for ModelInfo {
    fn from(model: omini_domain::config::ModelInfo) -> Self {
        Self {
            id: model.id,
            name: model.name,
            limit: model.limit,
            thinking: model.thinking,
            input_modalities: model
                .input_modalities
                .map(|items| items.into_iter().map(Into::into).collect()),
            extra_headers: model.extra_headers,
            extra_body: model.extra_body.map(|body| body.into_iter().collect()),
        }
    }
}

impl From<omini_domain::config::ProviderInfo> for ProviderInfo {
    fn from(provider: omini_domain::config::ProviderInfo) -> Self {
        Self {
            id: provider.id,
            name: provider.name,
            endpoint: provider.endpoint.into(),
            base_url: provider.base_url,
            models: provider.models.into_iter().map(Into::into).collect(),
        }
    }
}
