use crate::db::DbError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    OpenAI,
    Anthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingEffort {
    None,
    Low,
    #[default]
    Medium,
    High,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelConfig {
    /// 模型 ID
    pub id: String,
    /// 展示名称（可选，用于 TUI）
    pub name: Option<String>,
    /// 上下文长度限制（默认 256k，构建时已填充）
    pub limit: u32,
    /// 是否支持思考模式
    pub thinking: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderProfile {
    pub name: String,
    pub endpoint: ProviderType,
    pub api_key: String,
    pub base_url: String,
    pub models: Vec<ModelConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settings {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub endpoint: ProviderType,
    pub providers: HashMap<String, ProviderProfile>,
    pub active_provider: String,

    pub system_prompt: Option<String>,
    pub max_turns: Option<usize>,

    pub cwd: PathBuf,
    pub thinking_effort: Option<ThinkingEffort>,
}

// 配置错误
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no providers configured")]
    NoActiveProvider,
    #[error("provider '{0}' has no models configured")]
    NoModels(String),
    #[error("cannot read {path}: {source}")]
    ConfigLoad {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse {path}:\n  {source}")]
    ConfigParse {
        path: String,
        source: toml::de::Error,
    },
    #[error("failed to parse config toml: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to serialize toml: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database error: {0}")]
    Db(#[from] DbError),
}
