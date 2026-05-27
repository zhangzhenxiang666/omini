use crate::permissions::RawPermissionConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    OpenAI,
    Anthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingEffort {
    None,
    Low,
    #[default]
    Medium,
    High,
}

impl fmt::Display for ThinkingEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            ThinkingEffort::None => "none",
            ThinkingEffort::Low => "low",
            ThinkingEffort::Medium => "medium",
            ThinkingEffort::High => "high",
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
    /// Optional declared input modalities. `None` means undeclared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<InputModality>>,
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
    pub language: Option<String>,
    pub max_turns: Option<usize>,

    pub cwd: PathBuf,
    pub thinking_effort: Option<ThinkingEffort>,
    pub permissions: Option<RawPermissionConfig>,
    pub compact: CompactConfig,
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl Settings {
    pub fn current_model_config(&self) -> Option<&ModelConfig> {
        self.providers
            .get(&self.active_provider)
            .and_then(|provider| provider.models.iter().find(|model| model.id == self.model))
    }

    pub fn supports_input_modality(&self, modality: InputModality) -> bool {
        self.current_model_config()
            .and_then(|model| model.input_modalities.as_ref())
            .is_some_and(|modalities| modalities.contains(&modality))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpServerConfig {
    #[serde(flatten)]
    pub transport: McpServerTransportConfig,
    #[serde(default = "default_mcp_server_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_timeout_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_timeout_sec: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum McpServerTransportConfig {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
    StreamableHttp {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer_token_env_var: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        http_headers: Option<HashMap<String, String>>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMcpServerConfig {
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    url: Option<String>,
    #[serde(default)]
    bearer_token_env_var: Option<String>,
    #[serde(default)]
    http_headers: Option<HashMap<String, String>>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    startup_timeout_sec: Option<f64>,
    #[serde(default)]
    tool_timeout_sec: Option<f64>,
    #[serde(default)]
    enabled_tools: Option<Vec<String>>,
    #[serde(default)]
    disabled_tools: Option<Vec<String>>,
}

impl TryFrom<RawMcpServerConfig> for McpServerConfig {
    type Error = String;

    fn try_from(raw: RawMcpServerConfig) -> Result<Self, Self::Error> {
        let RawMcpServerConfig {
            command,
            args,
            env,
            cwd,
            url,
            bearer_token_env_var,
            http_headers,
            enabled,
            startup_timeout_sec,
            tool_timeout_sec,
            enabled_tools,
            disabled_tools,
        } = raw;

        let transport = match (command, url) {
            (Some(command), None) => {
                if bearer_token_env_var.is_some() {
                    return Err(
                        "bearer_token_env_var is not supported for stdio MCP servers".to_string(),
                    );
                }
                if http_headers.is_some() {
                    return Err("http_headers is not supported for stdio MCP servers".to_string());
                }
                McpServerTransportConfig::Stdio {
                    command,
                    args: args.unwrap_or_default(),
                    env,
                    cwd,
                }
            }
            (None, Some(url)) => {
                if args.is_some() {
                    return Err("args is not supported for streamable HTTP MCP servers".to_string());
                }
                if env.is_some() {
                    return Err("env is not supported for streamable HTTP MCP servers".to_string());
                }
                if cwd.is_some() {
                    return Err("cwd is not supported for streamable HTTP MCP servers".to_string());
                }
                McpServerTransportConfig::StreamableHttp {
                    url,
                    bearer_token_env_var,
                    http_headers,
                }
            }
            (Some(_), Some(_)) => {
                return Err(
                    "MCP server config must set either command or url, not both".to_string()
                );
            }
            (None, None) => {
                return Err("MCP server config must set command or url".to_string());
            }
        };

        Ok(Self {
            transport,
            enabled: enabled.unwrap_or_else(default_mcp_server_enabled),
            startup_timeout_sec,
            tool_timeout_sec,
            enabled_tools,
            disabled_tools,
        })
    }
}

impl<'de> Deserialize<'de> for McpServerConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RawMcpServerConfig::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

fn default_mcp_server_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompactConfig {
    #[serde(default = "default_compact_enabled")]
    pub enabled: bool,
    #[serde(default = "default_preserve_recent")]
    pub preserve_recent: usize,
    #[serde(default = "default_buffer_tokens")]
    pub buffer_tokens: usize,
    #[serde(default = "default_summary_output_tokens")]
    pub summary_output_tokens: usize,
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: usize,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            enabled: default_compact_enabled(),
            preserve_recent: default_preserve_recent(),
            buffer_tokens: default_buffer_tokens(),
            summary_output_tokens: default_summary_output_tokens(),
            max_consecutive_failures: default_max_consecutive_failures(),
        }
    }
}

fn default_compact_enabled() -> bool {
    true
}

fn default_preserve_recent() -> usize {
    6
}

fn default_buffer_tokens() -> usize {
    13_000
}

fn default_summary_output_tokens() -> usize {
    20_000
}

fn default_max_consecutive_failures() -> usize {
    3
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_config_input_modalities_can_be_undeclared() {
        let model: ModelConfig = serde_json::from_value(json!({
            "id": "gpt-test",
            "name": null,
            "limit": 256000,
            "thinking": false
        }))
        .unwrap();

        assert_eq!(model.input_modalities, None);
    }

    #[test]
    fn model_config_input_modalities_parse_image() {
        let model: ModelConfig = serde_json::from_value(json!({
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
}
