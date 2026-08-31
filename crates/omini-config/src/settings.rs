use crate::config::{EffectiveModelConfig, ModelSelection, ResolvedConfig, RoutingTier};
use indexmap::IndexMap;
use omini_domain::config::{InputModality, ThinkingEffort};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawPermissionConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Settings {
    resolved: Arc<ResolvedConfig>,
    active_model: EffectiveModelConfig,
    pub system_prompt: Option<String>,
    pub max_turns: Option<usize>,
    pub cwd: PathBuf,
}

impl Settings {
    pub(crate) fn new(
        resolved: Arc<ResolvedConfig>,
        active_model: EffectiveModelConfig,
        cwd: PathBuf,
    ) -> Self {
        Self {
            resolved,
            active_model,
            system_prompt: None,
            max_turns: None,
            cwd,
        }
    }

    pub fn resolved_config(&self) -> &ResolvedConfig {
        &self.resolved
    }

    pub fn active_model(&self) -> &EffectiveModelConfig {
        &self.active_model
    }

    pub fn resolve_model(
        &self,
        selection: &ModelSelection,
    ) -> Result<EffectiveModelConfig, ConfigError> {
        self.resolved.effective_model_config(selection)
    }

    pub fn select_model(&mut self, selection: ModelSelection) -> Result<(), ConfigError> {
        let active_model = self.resolve_model(&selection)?;
        self.active_model = active_model;
        Ok(())
    }

    pub fn set_thinking_effort(
        &mut self,
        thinking_effort: Option<ThinkingEffort>,
    ) -> Result<(), ConfigError> {
        self.select_model(ModelSelection {
            active_provider: self.active_model.provider_id.clone(),
            model: self.active_model.model_id.clone(),
            thinking_effort,
        })
    }

    pub fn resolve_routing_model(
        &self,
        tier: RoutingTier,
    ) -> Result<EffectiveModelConfig, ConfigError> {
        let current = ModelSelection {
            active_provider: self.active_model.provider_id.clone(),
            model: self.active_model.model_id.clone(),
            thinking_effort: self.active_model.thinking_effort,
        };
        let selection = self.resolved.routing_selection(tier, &current);
        self.resolved.effective_model_config(&selection)
    }

    pub fn supports_input_modality(&self, modality: InputModality) -> bool {
        self.active_model.capabilities.input.contains(&modality)
    }

    pub fn language(&self) -> Option<&str> {
        self.resolved.agent.language.as_deref()
    }

    pub fn permissions(&self) -> Option<RawPermissionConfig> {
        self.resolved.permissions.clone()
    }

    pub fn compact(&self) -> &CompactConfig {
        &self.resolved.context.compaction
    }

    pub fn mcp_servers(&self) -> &IndexMap<String, McpServerConfig> {
        &self.resolved.mcp
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
