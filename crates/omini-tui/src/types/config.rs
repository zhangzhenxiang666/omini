pub use omini_domain::config::{
    InputModality, ModelInfo as ModelConfig, ProviderEndpointKind as ProviderType, ThinkingEffort,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderProfile {
    pub name: String,
    pub endpoint: ProviderType,
    pub base_url: String,
    pub models: Vec<ModelConfig>,
}
