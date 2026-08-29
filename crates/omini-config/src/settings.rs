use omini_domain::config::{InputModality, ModelInfo, ProviderEndpointKind, ThinkingEffort};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use url::Url;

/// Provider-neutral 抽象模型档位。
///
/// 命名刻意不引用任何 vendor(haiku/sonnet/opus/mini/nano)，
/// 以保持跨 provider 配置可移植。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelTier {
    Small,
    Standard,
    Large,
}

impl ModelTier {
    pub const ALL: [ModelTier; 3] = [ModelTier::Small, ModelTier::Standard, ModelTier::Large];

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelTier::Small => "small",
            ModelTier::Standard => "standard",
            ModelTier::Large => "large",
        }
    }
}

/// 单个 tier 的配置定义:目标 provider + model + 可选 thinking effort。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelTierEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub thinking_effort: Option<ThinkingEffort>,
}

/// 完整的 `model_tiers` 配置块。
///
/// 整块缺失 = 所有 tier 在解析时 fallback 到当前线程模型。
/// 单独某个 slot 缺失 = 该 slot 解析时 fallback。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelTiers {
    #[serde(default)]
    pub small: Option<ModelTierEntry>,
    #[serde(default)]
    pub standard: Option<ModelTierEntry>,
    #[serde(default)]
    pub large: Option<ModelTierEntry>,
}

impl ModelTiers {
    pub fn get(&self, tier: ModelTier) -> Option<&ModelTierEntry> {
        match tier {
            ModelTier::Small => self.small.as_ref(),
            ModelTier::Standard => self.standard.as_ref(),
            ModelTier::Large => self.large.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawPermissionConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderProfile {
    pub name: String,
    pub endpoint: ProviderEndpointKind,
    pub api_key: String,
    pub base_url: Url,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settings {
    pub api_key: String,
    pub base_url: Url,
    pub model: String,
    pub endpoint: ProviderEndpointKind,
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
    #[serde(default)]
    pub model_tiers: ModelTiers,
}

// TODO: 需要重新审视方法的合理性
impl Settings {
    pub fn current_model_config(&self) -> Option<&ModelInfo> {
        self.providers
            .get(&self.active_provider)
            .and_then(|provider| provider.models.iter().find(|model| model.id == self.model))
    }

    pub fn model_supports_thinking(&self, provider: &str, model: &str) -> bool {
        self.providers
            .get(provider)
            .and_then(|profile| {
                profile
                    .models
                    .iter()
                    .find(|candidate| candidate.id == model)
            })
            .is_some_and(|model| model.thinking)
    }

    pub fn current_model_supports_thinking(&self) -> bool {
        self.model_supports_thinking(&self.active_provider, &self.model)
    }

    pub fn effective_thinking_effort_for(
        &self,
        provider: &str,
        model: &str,
        effort: Option<ThinkingEffort>,
    ) -> Option<ThinkingEffort> {
        if !self.model_supports_thinking(provider, model) {
            return None;
        }

        Some(effort.unwrap_or_default())
    }

    pub fn effective_current_thinking_effort(
        &self,
        effort: Option<ThinkingEffort>,
    ) -> Option<ThinkingEffort> {
        self.effective_thinking_effort_for(&self.active_provider, &self.model, effort)
    }

    pub fn normalize_current_thinking_effort(&mut self) {
        self.thinking_effort = self.effective_current_thinking_effort(self.thinking_effort);
    }

    pub fn supports_input_modality(&self, modality: InputModality) -> bool {
        self.current_model_config()
            .and_then(|model| model.input_modalities.as_ref())
            .is_some_and(|modalities| modalities.contains(&modality))
    }

    /// 解析指定档位应使用的 `(provider, model, thinking_effort)`。
    ///
    /// 任一前置条件不满足时,fallback 到当前线程活跃 provider/model
    /// 并保留当前 `thinking_effort`,同时记录 `tracing::warn`:
    ///   1. `model_tiers` 未配置 / 该 tier slot 未配置;
    ///   2. tier.provider 不在 `self.providers`;
    ///   3. tier.model 不在该 provider.models;
    ///   4. target model 不支持 thinking → effort 归一化为 `None`。
    ///
    /// 思考力度按 `effective_thinking_effort_for` 归一化,与主线程模型
    /// 选择行为一致。
    pub fn resolve_tier(&self, tier: ModelTier) -> (String, String, Option<ThinkingEffort>) {
        let entry = match self.model_tiers.get(tier) {
            Some(e) => e,
            None => {
                return self.fallback_for_tier(tier, "tier_not_configured", None, None);
            }
        };
        if !self.providers.contains_key(&entry.provider) {
            return self.fallback_for_tier(
                tier,
                "tier_provider_missing",
                Some(&entry.provider),
                None,
            );
        }
        let model_exists = self.providers[&entry.provider]
            .models
            .iter()
            .any(|m| m.id == entry.model);
        if !model_exists {
            return self.fallback_for_tier(
                tier,
                "tier_model_missing",
                Some(&entry.provider),
                Some(&entry.model),
            );
        }
        let effort = self.effective_thinking_effort_for(
            &entry.provider,
            &entry.model,
            entry.thinking_effort,
        );
        (entry.provider.clone(), entry.model.clone(), effort)
    }

    fn fallback_for_tier(
        &self,
        tier: ModelTier,
        reason: &'static str,
        configured_provider: Option<&str>,
        configured_model: Option<&str>,
    ) -> (String, String, Option<ThinkingEffort>) {
        tracing::warn!(
            tier = tier.as_str(),
            reason = reason,
            configured_provider = configured_provider.unwrap_or("-"),
            configured_model = configured_model.unwrap_or("-"),
            active_provider = %self.active_provider,
            active_model = %self.model,
            "model tier resolution fell back to current thread model",
        );
        (
            self.active_provider.clone(),
            self.model.clone(),
            self.thinking_effort,
        )
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

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("provider id cannot be empty")]
    InvalidProviderId,
    #[error("model id cannot be empty for provider '{provider}'")]
    InvalidModelId { provider: String },
    #[error("provider '{provider}' must set '{field}'")]
    MissingProviderField {
        provider: String,
        field: &'static str,
    },
    #[error("provider '{provider}' has an invalid base_url: {source}")]
    InvalidBaseUrl {
        provider: String,
        source: url::ParseError,
    },
    #[error("model '{model}' for provider '{provider}' must have a non-zero context_window")]
    InvalidContextWindow { provider: String, model: String },
    #[error("environment variable '{0}' is required by config")]
    MissingEnv(String),
    #[error("unknown provider '{0}'")]
    UnknownProvider(String),
    #[error("unknown model '{model}' for provider '{provider}'")]
    UnknownModel { provider: String, model: String },
    #[error("routing references unknown provider '{0}'")]
    UnknownRoutingProvider(String),
    #[error("routing references unknown model '{model}' for provider '{provider}'")]
    UnknownRoutingModel { provider: String, model: String },
    #[error("no providers configured")]
    NoActiveProvider,
    #[error("provider '{0}' has no models configured")]
    NoModels(String),
    #[error("project provider '{provider}' must set '{field}' when adding a new provider")]
    ProjectProviderFieldRequired {
        provider: String,
        field: &'static str,
    },
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
    #[error("cannot edit {path}: {source}")]
    ConfigEdit {
        path: PathBuf,
        source: toml_edit::TomlError,
    },
    #[error("bootstrap expected TOML table at '{0}'")]
    BootstrapTableConflict(String),
    #[error("cannot read auth file {path}: {source}")]
    AuthLoad {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse auth file {path}: {source}")]
    AuthParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("auth environment variable name must not be empty")]
    InvalidAuthEnvironmentVariable,
    #[error("auth file path has no parent: {0}")]
    InvalidAuthPath(PathBuf),
    #[error("failed to parse config toml: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to serialize toml: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct OminiRoot {
    path: PathBuf,
}

impl OminiRoot {
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn init() -> std::io::Result<Self> {
        let path = dirs::home_dir()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "cannot find home dir")
            })?
            .join(".omini");

        if !path.exists() {
            std::fs::create_dir_all(&path)?;
        }

        Ok(Self { path })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn config_path(&self) -> PathBuf {
        self.path.join("config.toml")
    }

    pub fn auth_path(&self) -> PathBuf {
        self.path.join("auth.json")
    }

    pub fn load_auth_environment(&self) -> Result<crate::AuthEnvironment, ConfigError> {
        Ok(crate::AuthStore::load(&self.auth_path())?.environment())
    }

    pub fn project_config_path(&self, cwd: &Path) -> PathBuf {
        cwd.join(".omini").join("config.toml")
    }

    pub fn db_path(&self) -> PathBuf {
        self.path.join("omini.db")
    }

    pub fn load_config(&self) -> Result<crate::RawConfig, ConfigError> {
        let path = self.config_path();
        load_toml_file(&path)
    }

    pub fn load_project_config(&self, cwd: &Path) -> Result<Option<crate::RawConfig>, ConfigError> {
        let path = self.project_config_path(cwd);
        if !path.exists() {
            return Ok(None);
        }
        load_toml_file(&path).map(Some)
    }

    pub fn load_config_for_cwd(&self, cwd: &Path) -> Result<crate::ResolvedConfig, ConfigError> {
        crate::load_resolved_config_for_cwd(self, cwd)
    }

    pub fn projects_dir(&self) -> crate::project::ProjectsDir {
        crate::project::ProjectsDir::new(&self.path)
    }

    pub fn init_project(
        &self,
        storage_key: &str,
        config: &crate::ResolvedConfig,
    ) -> Result<crate::project::ProjectDir, ConfigError> {
        self.projects_dir().for_storage_key(storage_key, config)
    }
}

fn load_toml_file<T>(path: &Path) -> Result<T, ConfigError>
where
    T: for<'de> Deserialize<'de>,
{
    let path_str = path.display().to_string();
    let content = std::fs::read_to_string(path).map_err(|e| ConfigError::ConfigLoad {
        path: path.to_path_buf(),
        source: e,
    })?;
    toml::from_str(&content).map_err(|e| ConfigError::ConfigParse {
        path: path_str,
        source: e,
    })
}
