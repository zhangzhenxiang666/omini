use omini_domain::config::{InputModality, ModelInfo, ProviderEndpointKind, ThinkingEffort};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
/// 整块缺失 = 所有 tier 在解析时 fallback 到当前会话模型。
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

/// Project overlay 使用的整体替换变体,语义与 `PartialCompactConfig` 一致:
/// 项目级只声明需要覆盖的 slot,未声明的 slot 保留用户级。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialModelTiers {
    pub small: Option<ModelTierEntry>,
    pub standard: Option<ModelTierEntry>,
    pub large: Option<ModelTierEntry>,
}

impl PartialModelTiers {
    /// 子表整体替换:任意 slot 存在就替换该 slot,其余保留 target 现状。
    pub fn merge_into(self, target: &mut ModelTiers) {
        if let Some(v) = self.small {
            target.small = Some(v);
        }
        if let Some(v) = self.standard {
            target.standard = Some(v);
        }
        if let Some(v) = self.large {
            target.large = Some(v);
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
    pub base_url: String,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settings {
    pub api_key: String,
    pub base_url: String,
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
    /// 任一前置条件不满足时,fallback 到当前会话活跃 provider/model
    /// 并保留当前 `thinking_effort`,同时记录 `tracing::warn`:
    ///   1. `model_tiers` 未配置 / 该 tier slot 未配置;
    ///   2. tier.provider 不在 `self.providers`;
    ///   3. tier.model 不在该 provider.models;
    ///   4. target model 不支持 thinking → effort 归一化为 `None`。
    ///
    /// 思考力度按 `effective_thinking_effort_for` 归一化,与主会话模型
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
            "model tier resolution fell back to current session model",
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
    #[error("failed to parse config toml: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to serialize toml: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserConfig {
    pub providers: HashMap<String, ProviderConfig>,
    pub language: Option<String>,
    pub permissions: Option<RawPermissionConfig>,
    pub compact: Option<CompactConfig>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// 可选模型分级配置;缺失 = 所有 tier fallback 当前会话模型。
    #[serde(default)]
    pub model_tiers: ModelTiers,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub name: Option<String>,
    pub endpoint: ProviderEndpointKind,
    pub base_url: String,
    pub api_key: String,
    pub models: Option<HashMap<String, ModelEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub name: Option<String>,
    pub limit: Option<u32>,
    pub thinking: Option<bool>,
    pub input_modalities: Option<Vec<InputModality>>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub body: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PartialUserConfig {
    #[serde(default)]
    pub providers: HashMap<String, PartialProviderConfig>,
    pub language: Option<String>,
    pub permissions: Option<PartialRawPermissionConfig>,
    pub compact: Option<PartialCompactConfig>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    pub model_tiers: Option<PartialModelTiers>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PartialProviderConfig {
    pub name: Option<String>,
    pub endpoint: Option<ProviderEndpointKind>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    #[serde(default)]
    pub models: HashMap<String, PartialModelEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialModelEntry {
    pub name: Option<String>,
    pub limit: Option<u32>,
    pub thinking: Option<bool>,
    pub input_modalities: Option<Vec<InputModality>>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub body: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PartialRawPermissionConfig {
    pub allow: Option<Vec<String>>,
    pub ask: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PartialCompactConfig {
    pub enabled: Option<bool>,
    pub preserve_recent: Option<usize>,
    pub buffer_tokens: Option<usize>,
    pub summary_output_tokens: Option<usize>,
    pub max_consecutive_failures: Option<usize>,
}

impl UserConfig {
    pub fn merge_project_config(&mut self, project: PartialUserConfig) -> Result<(), ConfigError> {
        // TODO(#15): 当前合并是“整体吞下 + 字段级覆盖”，没有 diagnostics 也没有中间快照。
        // 后续可以加：合并过程产出 diagnostic（被覆盖/被忽略字段、来源标记），
        // 并在 validate() 失败时返回合并上下文（用户级/项目级路径、字段来源），
        // 避免出现不可解释的“配置被静默改写”。
        if let Some(language) = project.language {
            self.language = Some(language);
        }
        if let Some(permissions) = project.permissions {
            let base = self.permissions.take().unwrap_or_default();
            self.permissions = Some(permissions.merge_into(base));
        }
        if let Some(compact) = project.compact {
            let base = self.compact.take().unwrap_or_default();
            self.compact = Some(compact.merge_into(base));
        }
        for (name, provider) in project.providers {
            match self.providers.get_mut(&name) {
                Some(base) => provider.merge_into(base),
                None => {
                    self.providers
                        .insert(name.clone(), provider.into_provider_config(&name)?);
                }
            }
        }
        for (name, server) in project.mcp_servers {
            // TODO(#15): 当前同名 mcp_server 整体覆盖（plan 中明确第一版不做
            // transport 字段级混合，避免 command/url 半合并产生非法状态）。
            // 后续可以在引入 trusted project 机制后，再考虑按 transport 字段
            // 拆分合并并输出 diagnostic。
            self.mcp_servers.insert(name, server);
        }
        if let Some(partial) = project.model_tiers {
            partial.merge_into(&mut self.model_tiers);
        }
        Ok(())
    }

    pub fn to_settings(
        &self,
        active_provider: Option<&str>,
        active_model: Option<&str>,
        thinking_effort: Option<ThinkingEffort>,
    ) -> Result<Settings, ConfigError> {
        let active_name = active_provider
            .and_then(|name| self.providers.get(name))
            .map(|_| active_provider.unwrap())
            .or_else(|| self.providers.keys().next().map(|s| s.as_str()))
            .ok_or(ConfigError::NoActiveProvider)?;
        let active = &self.providers[active_name];

        let mut providers = HashMap::new();
        for (name, pc) in &self.providers {
            let models = pc
                .models
                .as_ref()
                .map(|m| {
                    m.iter()
                        .map(|(id, entry)| ModelInfo {
                            id: id.clone(),
                            name: entry.name.clone(),
                            limit: entry.limit.unwrap_or(256000),
                            thinking: entry.thinking.unwrap_or(false),
                            input_modalities: entry.input_modalities.clone(),
                            extra_headers: entry.headers.clone(),
                            extra_body: entry.body.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();

            providers.insert(
                name.clone(),
                ProviderProfile {
                    name: pc.name.clone().unwrap_or_else(|| name.clone()),
                    endpoint: pc.endpoint,
                    api_key: pc.api_key.clone(),
                    base_url: pc.base_url.clone(),
                    models,
                },
            );
        }

        let first_model = providers[active_name]
            .models
            .first()
            .map(|m| m.id.clone())
            .unwrap_or_default();

        let model = active_model
            .filter(|m| providers[active_name].models.iter().any(|mc| mc.id == *m))
            .unwrap_or(&first_model)
            .to_string();

        let mut settings = Settings {
            api_key: active.api_key.clone(),
            base_url: active.base_url.clone(),
            model,
            endpoint: active.endpoint,
            providers,
            active_provider: active_name.to_owned(),
            system_prompt: None,
            language: self.language.clone(),
            max_turns: None,
            cwd: std::env::current_dir()?,
            thinking_effort,
            permissions: self.permissions.clone(),
            compact: self.compact.clone().unwrap_or_default(),
            mcp_servers: self.mcp_servers.clone(),
            model_tiers: self.model_tiers.clone(),
        };
        settings.normalize_current_thinking_effort();
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.providers.is_empty() {
            return Err(ConfigError::NoActiveProvider);
        }
        for (name, pc) in &self.providers {
            let has_models = pc.models.as_ref().is_some_and(|m| !m.is_empty());
            if !has_models {
                return Err(ConfigError::NoModels(name.clone()));
            }
        }
        Ok(())
    }
}

impl PartialProviderConfig {
    fn merge_into(self, base: &mut ProviderConfig) {
        if let Some(name) = self.name {
            base.name = Some(name);
        }
        if let Some(endpoint) = self.endpoint {
            base.endpoint = endpoint;
        }
        if let Some(base_url) = self.base_url {
            base.base_url = base_url;
        }
        if let Some(api_key) = self.api_key {
            base.api_key = api_key;
        }
        if !self.models.is_empty() {
            let models = base.models.get_or_insert_with(HashMap::new);
            for (id, model) in self.models {
                match models.get_mut(&id) {
                    Some(base) => model.merge_into(base),
                    None => {
                        models.insert(id, model.into_model_entry());
                    }
                }
            }
        }
    }

    fn into_provider_config(self, provider: &str) -> Result<ProviderConfig, ConfigError> {
        let endpoint = self
            .endpoint
            .ok_or_else(|| ConfigError::ProjectProviderFieldRequired {
                provider: provider.to_string(),
                field: "endpoint",
            })?;
        let base_url = self
            .base_url
            .ok_or_else(|| ConfigError::ProjectProviderFieldRequired {
                provider: provider.to_string(),
                field: "base_url",
            })?;
        let api_key = self
            .api_key
            .ok_or_else(|| ConfigError::ProjectProviderFieldRequired {
                provider: provider.to_string(),
                field: "api_key",
            })?;
        let models = if self.models.is_empty() {
            None
        } else {
            Some(
                self.models
                    .into_iter()
                    .map(|(id, model)| (id, model.into_model_entry()))
                    .collect(),
            )
        };
        Ok(ProviderConfig {
            name: self.name,
            endpoint,
            base_url,
            api_key,
            models,
        })
    }
}

impl PartialModelEntry {
    fn merge_into(self, base: &mut ModelEntry) {
        if let Some(name) = self.name {
            base.name = Some(name);
        }
        if let Some(limit) = self.limit {
            base.limit = Some(limit);
        }
        if let Some(thinking) = self.thinking {
            base.thinking = Some(thinking);
        }
        if let Some(input_modalities) = self.input_modalities {
            base.input_modalities = Some(input_modalities);
        }
        if let Some(headers) = self.headers {
            base.headers = Some(headers);
        }
        if let Some(body) = self.body {
            base.body = Some(body);
        }
    }

    fn into_model_entry(self) -> ModelEntry {
        ModelEntry {
            name: self.name,
            limit: self.limit,
            thinking: self.thinking,
            input_modalities: self.input_modalities,
            headers: self.headers,
            body: self.body,
        }
    }
}

impl PartialRawPermissionConfig {
    fn merge_into(self, mut base: RawPermissionConfig) -> RawPermissionConfig {
        // TODO(#15): 当前 allow/ask/deny 任一字段被项目设置就直接整段覆盖，
        // 不会做“用户级 + 项目级”合并/去重，也不会校验优先级。
        // 后续权限层（第三阶段）需要决定语义：
        //   1) 项目级整体替换（当前行为），
        //   2) 项目级附加到对应列表后再去重（注意 deny 永远 win），
        //   3) 同时存在 .omini/permissions.toml 时的合并顺序 + diagnostic。
        // 建议在迁出 permissions 来源发现/解析时一起决定，不要在配置层先拍板。
        if let Some(allow) = self.allow {
            base.allow = allow;
        }
        if let Some(ask) = self.ask {
            base.ask = ask;
        }
        if let Some(deny) = self.deny {
            base.deny = deny;
        }
        base
    }
}

impl PartialCompactConfig {
    fn merge_into(self, mut base: CompactConfig) -> CompactConfig {
        if let Some(enabled) = self.enabled {
            base.enabled = enabled;
        }
        if let Some(preserve_recent) = self.preserve_recent {
            base.preserve_recent = preserve_recent;
        }
        if let Some(buffer_tokens) = self.buffer_tokens {
            base.buffer_tokens = buffer_tokens;
        }
        if let Some(summary_output_tokens) = self.summary_output_tokens {
            base.summary_output_tokens = summary_output_tokens;
        }
        if let Some(max_consecutive_failures) = self.max_consecutive_failures {
            base.max_consecutive_failures = max_consecutive_failures;
        }
        base
    }
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

    pub fn project_config_path(&self, cwd: &Path) -> PathBuf {
        cwd.join(".omini").join("config.toml")
    }

    pub fn db_path(&self) -> PathBuf {
        self.path.join("omini.db")
    }

    pub fn load_config(&self) -> Result<UserConfig, ConfigError> {
        let path = self.config_path();
        load_toml_file(&path)
    }

    pub fn load_project_config(
        &self,
        cwd: &Path,
    ) -> Result<Option<PartialUserConfig>, ConfigError> {
        let path = self.project_config_path(cwd);
        if !path.exists() {
            return Ok(None);
        }
        load_toml_file(&path).map(Some)
    }

    pub fn load_config_for_cwd(&self, cwd: &Path) -> Result<UserConfig, ConfigError> {
        let mut config = self.load_config()?;
        if let Some(project_config) = self.load_project_config(cwd)? {
            config.merge_project_config(project_config)?;
        }
        Ok(config)
    }

    pub fn projects_dir(&self) -> crate::project::ProjectsDir {
        crate::project::ProjectsDir::new(&self.path)
    }

    pub fn init_project(
        &self,
        storage_key: &str,
        config: &UserConfig,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn config_from_toml(content: &str) -> UserConfig {
        toml::from_str(content).expect("config should parse")
    }

    fn partial_from_toml(content: &str) -> PartialUserConfig {
        toml::from_str(content).expect("partial config should parse")
    }

    fn minimal_config(language_line: &str) -> UserConfig {
        config_from_toml(&format!(
            r#"{language_line}
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = {{ name = "GPT Test" }}
"#
        ))
    }

    #[test]
    fn language_reaches_runtime_settings_without_normalization() {
        let config = minimal_config(r#"language = "  简体中文  ""#);

        let settings = config.to_settings(None, None, None).unwrap();

        assert_eq!(settings.language.as_deref(), Some("  简体中文  "));
    }

    #[test]
    fn missing_language_stays_unset() {
        let config = minimal_config("");

        let settings = config.to_settings(None, None, None).unwrap();

        assert_eq!(settings.language, None);
    }

    #[test]
    fn model_entry_input_modalities_reach_settings() {
        let config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = { input_modalities = ["text", "image"] }
"#,
        );

        let settings = config.to_settings(None, None, None).unwrap();
        let model = settings.current_model_config().unwrap();

        assert_eq!(
            model.input_modalities.as_deref(),
            Some(&[InputModality::Text, InputModality::Image][..])
        );
        assert!(settings.supports_input_modality(InputModality::Image));
    }

    #[test]
    fn quoted_dotted_model_id_parses_as_single_model_id() {
        let config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models."gpt-4.1"]
name = "GPT 4.1"
"#,
        );

        let settings = config
            .to_settings(Some("openai"), Some("gpt-4.1"), None)
            .unwrap();

        assert_eq!(settings.model, "gpt-4.1");
        assert_eq!(
            settings
                .current_model_config()
                .and_then(|model| model.name.as_deref()),
            Some("GPT 4.1")
        );
    }

    #[test]
    fn unquoted_dotted_model_id_is_rejected() {
        let error = toml::from_str::<UserConfig>(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models.gpt-4.1]
name = "GPT 4.1"
"#,
        )
        .expect_err("unquoted dotted model ids should be rejected");

        let message = error.to_string();
        assert!(message.contains("unknown field"));
        assert!(message.contains("1"));
    }

    #[test]
    fn to_settings_clears_thinking_effort_for_non_thinking_model() {
        let config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
fast = {}
reasoner = { thinking = true }
"#,
        );

        let settings = config
            .to_settings(Some("openai"), Some("fast"), Some(ThinkingEffort::Medium))
            .unwrap();

        assert!(!settings.current_model_supports_thinking());
        assert_eq!(settings.thinking_effort, None);
    }

    #[test]
    fn to_settings_keeps_thinking_effort_for_thinking_model() {
        let config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
fast = {}
reasoner = { thinking = true }
"#,
        );

        let settings = config
            .to_settings(Some("openai"), Some("reasoner"), Some(ThinkingEffort::High))
            .unwrap();

        assert!(settings.current_model_supports_thinking());
        assert_eq!(settings.thinking_effort, Some(ThinkingEffort::High));
    }

    #[test]
    fn to_settings_defaults_thinking_model_effort_to_medium() {
        let config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
fast = {}
reasoner = { thinking = true }
"#,
        );

        let settings = config
            .to_settings(Some("openai"), Some("reasoner"), None)
            .unwrap();

        assert!(settings.current_model_supports_thinking());
        assert_eq!(settings.thinking_effort, Some(ThinkingEffort::Medium));
    }

    #[test]
    fn mcp_servers_reach_runtime_settings() {
        let config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = { name = "GPT Test" }

[mcp_servers.docs]
command = "docs-server"
args = ["--stdio"]
startup_timeout_sec = 1.5
enabled_tools = ["search"]

[mcp_servers.remote]
url = "https://example.test/mcp"
bearer_token_env_var = "TOKEN_ENV"
enabled = false
"#,
        );

        let settings = config.to_settings(None, None, None).unwrap();

        let docs = settings.mcp_servers.get("docs").unwrap();
        assert!(docs.enabled);
        assert_eq!(docs.startup_timeout_sec, Some(1.5));
        assert_eq!(
            docs.enabled_tools.as_deref(),
            Some(&["search".to_string()][..])
        );
        assert!(matches!(
            docs.transport,
            McpServerTransportConfig::Stdio { .. }
        ));

        let remote = settings.mcp_servers.get("remote").unwrap();
        assert!(!remote.enabled);
        assert!(matches!(
            remote.transport,
            McpServerTransportConfig::StreamableHttp { .. }
        ));
    }

    #[test]
    fn mcp_server_rejects_invalid_transport_mix() {
        let error = toml::from_str::<UserConfig>(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = { name = "GPT Test" }

[mcp_servers.bad]
command = "docs-server"
url = "https://example.test/mcp"
"#,
        )
        .expect_err("config should reject command plus url");

        assert!(error.to_string().contains("either command or url"));
    }

    #[test]
    fn project_config_overrides_top_level_fields() {
        let mut config = minimal_config(r#"language = "English""#);
        config
            .merge_project_config(partial_from_toml(
                r#"
language = "简体中文"
"#,
            ))
            .unwrap();

        assert_eq!(config.language.as_deref(), Some("简体中文"));
    }

    #[test]
    fn project_config_merges_provider_and_model_maps_by_key() {
        let mut config = config_from_toml(
            r#"
[providers.openai]
name = "OpenAI"
endpoint = "openai"
base_url = "https://user.example"
api_key = "user-key"

[providers.openai.models.fast]
name = "Fast"
limit = 1000
thinking = false

[providers.openai.models.legacy]
name = "Legacy"
"#,
        );

        config
            .merge_project_config(partial_from_toml(
                r#"
[providers.openai]
base_url = "https://project.example"

[providers.openai.models.fast]
name = "Project Fast"
limit = 2000

[providers.openai.models.reasoner]
thinking = true
"#,
            ))
            .unwrap();

        let provider = config.providers.get("openai").unwrap();
        assert_eq!(provider.endpoint, ProviderEndpointKind::OpenAI);
        assert_eq!(provider.api_key, "user-key");
        assert_eq!(provider.base_url, "https://project.example");
        let models = provider.models.as_ref().unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models["fast"].name.as_deref(), Some("Project Fast"));
        assert_eq!(models["fast"].limit, Some(2000));
        assert_eq!(models["fast"].thinking, Some(false));
        assert_eq!(models["legacy"].name.as_deref(), Some("Legacy"));
        assert_eq!(models["reasoner"].thinking, Some(true));
    }

    #[test]
    fn project_config_can_add_complete_provider() {
        let mut config = minimal_config("");

        config
            .merge_project_config(partial_from_toml(
                r#"
[providers.anthropic]
endpoint = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models.claude-test]
name = "Claude Test"
"#,
            ))
            .unwrap();

        assert!(config.providers.contains_key("openai"));
        assert!(config.providers.contains_key("anthropic"));
        config.validate().unwrap();
    }

    #[test]
    fn project_config_rejects_incomplete_new_provider() {
        let mut config = minimal_config("");

        let error = config
            .merge_project_config(partial_from_toml(
                r#"
[providers.anthropic]
base_url = "https://anthropic.example"
api_key = "anthropic-key"
"#,
            ))
            .expect_err("new provider should require endpoint");

        assert!(matches!(
            error,
            ConfigError::ProjectProviderFieldRequired {
                provider,
                field: "endpoint"
            } if provider == "anthropic"
        ));
    }

    #[test]
    fn project_config_replaces_mcp_servers_by_key() {
        let mut config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = {}

[mcp_servers.docs]
command = "user-docs"
args = ["--user"]

[mcp_servers.remote]
url = "https://example.test/mcp"
"#,
        );

        config
            .merge_project_config(partial_from_toml(
                r#"
[mcp_servers.docs]
command = "project-docs"
enabled = false
"#,
            ))
            .unwrap();

        let docs = config.mcp_servers.get("docs").unwrap();
        assert!(!docs.enabled);
        assert!(matches!(
            &docs.transport,
            McpServerTransportConfig::Stdio { command, args, .. }
            if command == "project-docs" && args.is_empty()
        ));
        assert!(config.mcp_servers.contains_key("remote"));
    }

    #[test]
    fn project_config_merges_compact_and_permissions_by_field() {
        let mut config = config_from_toml(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = {}

[permissions]
allow = ["read"]
ask = ["edit"]

[compact]
enabled = true
preserve_recent = 4
"#,
        );

        config
            .merge_project_config(partial_from_toml(
                r#"
[permissions]
deny = ["write"]

[compact]
enabled = false
buffer_tokens = 5000
"#,
            ))
            .unwrap();

        let permissions = config.permissions.unwrap();
        assert_eq!(permissions.allow, ["read"]);
        assert_eq!(permissions.ask, ["edit"]);
        assert_eq!(permissions.deny, ["write"]);

        let compact = config.compact.unwrap();
        assert!(!compact.enabled);
        assert_eq!(compact.preserve_recent, 4);
        assert_eq!(compact.buffer_tokens, 5000);
    }

    #[test]
    fn load_config_for_cwd_ignores_missing_project_config() {
        let root_path = std::env::temp_dir().join(format!(
            "omini-config-missing-project-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let cwd = root_path.join("project");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            root_path.join("config.toml"),
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = {}
"#,
        )
        .unwrap();
        let root = OminiRoot::from_path(root_path.clone());

        let config = root.load_config_for_cwd(&cwd).unwrap();

        assert_eq!(
            config.providers["openai"].base_url,
            "https://openai.example"
        );
        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn load_config_for_cwd_applies_project_config_when_present() {
        let root_path = std::env::temp_dir().join(format!(
            "omini-config-project-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let cwd = root_path.join("project");
        fs::create_dir_all(cwd.join(".omini")).unwrap();
        fs::write(
            root_path.join("config.toml"),
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models]
gpt-test = {}
"#,
        )
        .unwrap();
        fs::write(
            cwd.join(".omini").join("config.toml"),
            r#"
language = "简体中文"

[providers.openai]
base_url = "https://project.example"
"#,
        )
        .unwrap();
        let root = OminiRoot::from_path(root_path.clone());

        let config = root.load_config_for_cwd(&cwd).unwrap();

        assert_eq!(config.language.as_deref(), Some("简体中文"));
        assert_eq!(
            config.providers["openai"].base_url,
            "https://project.example"
        );
        let _ = fs::remove_dir_all(root_path);
    }

    // ── model_tiers 测试 (issue #19) ─────────────────────────────

    fn two_provider_config_with_tiers(model_tiers_block: &str) -> String {
        format!(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "openai-key"

[providers.openai.models]
fast = {{}}
reasoner = {{ thinking = true }}

[providers.anthropic]
endpoint = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models]
haiku = {{}}
opus = {{ thinking = true }}

{model_tiers_block}
"#
        )
    }

    // 6.1 Schema 解析

    #[test]
    fn model_tiers_omitted_defaults_to_all_none() {
        let config = config_from_toml(&two_provider_config_with_tiers(""));

        assert!(config.model_tiers.small.is_none());
        assert!(config.model_tiers.standard.is_none());
        assert!(config.model_tiers.large.is_none());

        let settings = config.to_settings(None, None, None).unwrap();
        assert!(settings.model_tiers.small.is_none());
        assert!(settings.model_tiers.standard.is_none());
        assert!(settings.model_tiers.large.is_none());
    }

    #[test]
    fn model_tiers_partial_parses_only_configured_slots() {
        let config = config_from_toml(&two_provider_config_with_tiers(
            r#"
[model_tiers.small]
provider = "openai"
model = "fast"
"#,
        ));

        let small = config.model_tiers.small.as_ref().expect("small slot");
        assert_eq!(small.provider, "openai");
        assert_eq!(small.model, "fast");
        assert!(small.thinking_effort.is_none());

        assert!(config.model_tiers.standard.is_none());
        assert!(config.model_tiers.large.is_none());
    }

    #[test]
    fn model_tiers_all_three_slots_parse() {
        let config = config_from_toml(&two_provider_config_with_tiers(
            r#"
[model_tiers.small]
provider = "anthropic"
model = "haiku"
thinking_effort = "low"

[model_tiers.standard]
provider = "openai"
model = "fast"

[model_tiers.large]
provider = "anthropic"
model = "opus"
thinking_effort = "high"
"#,
        ));

        let small = config.model_tiers.small.as_ref().unwrap();
        assert_eq!(small.provider, "anthropic");
        assert_eq!(small.model, "haiku");
        assert_eq!(small.thinking_effort, Some(ThinkingEffort::Low));

        let standard = config.model_tiers.standard.as_ref().unwrap();
        assert_eq!(standard.provider, "openai");
        assert_eq!(standard.model, "fast");
        assert_eq!(standard.thinking_effort, None);

        let large = config.model_tiers.large.as_ref().unwrap();
        assert_eq!(large.provider, "anthropic");
        assert_eq!(large.model, "opus");
        assert_eq!(large.thinking_effort, Some(ThinkingEffort::High));
    }

    #[test]
    fn model_tiers_rejects_unknown_tier_slot_name() {
        let error = toml::from_str::<UserConfig>(&two_provider_config_with_tiers(
            r#"
[model_tiers.unknown]
provider = "openai"
model = "fast"
"#,
        ))
        .expect_err("model_tiers.unknown should be rejected");

        assert!(error.to_string().contains("unknown"));
    }

    #[test]
    fn model_tiers_rejects_unknown_field_in_entry() {
        let error = toml::from_str::<UserConfig>(&two_provider_config_with_tiers(
            r#"
[model_tiers.small]
provider = "openai"
model = "fast"
extra = "x"
"#,
        ))
        .expect_err("unknown field in entry should be rejected");

        assert!(error.to_string().contains("extra"));
    }

    #[test]
    fn model_tiers_thinking_effort_accepts_lowercase_strings() {
        let config = config_from_toml(&two_provider_config_with_tiers(
            r#"
[model_tiers.small]
provider = "openai"
model = "fast"
thinking_effort = "low"
"#,
        ));

        assert_eq!(
            config.model_tiers.small.unwrap().thinking_effort,
            Some(ThinkingEffort::Low)
        );
    }

    // 6.2 Partial / merge

    #[test]
    fn partial_model_tiers_merge_into_replaces_set_slots() {
        let mut target = ModelTiers {
            small: Some(ModelTierEntry {
                provider: "openai".into(),
                model: "fast".into(),
                thinking_effort: None,
            }),
            standard: Some(ModelTierEntry {
                provider: "openai".into(),
                model: "reasoner".into(),
                thinking_effort: Some(ThinkingEffort::High),
            }),
            large: Some(ModelTierEntry {
                provider: "anthropic".into(),
                model: "opus".into(),
                thinking_effort: Some(ThinkingEffort::Max),
            }),
        };

        let partial = PartialModelTiers {
            small: Some(ModelTierEntry {
                provider: "anthropic".into(),
                model: "haiku".into(),
                thinking_effort: Some(ThinkingEffort::Low),
            }),
            standard: None,
            large: None,
        };

        partial.merge_into(&mut target);

        let small = target.small.unwrap();
        assert_eq!(small.provider, "anthropic");
        assert_eq!(small.model, "haiku");

        let standard = target.standard.unwrap();
        assert_eq!(standard.provider, "openai");
        assert_eq!(standard.model, "reasoner");
        assert_eq!(standard.thinking_effort, Some(ThinkingEffort::High));

        let large = target.large.unwrap();
        assert_eq!(large.provider, "anthropic");
        assert_eq!(large.model, "opus");
        assert_eq!(large.thinking_effort, Some(ThinkingEffort::Max));
    }

    #[test]
    fn partial_model_tiers_merge_into_with_no_partial_keeps_target() {
        let mut target = ModelTiers {
            small: Some(ModelTierEntry {
                provider: "openai".into(),
                model: "fast".into(),
                thinking_effort: None,
            }),
            ..Default::default()
        };

        PartialModelTiers::default().merge_into(&mut target);

        let small = target.small.unwrap();
        assert_eq!(small.provider, "openai");
        assert_eq!(small.model, "fast");
    }

    #[test]
    fn user_config_merge_project_config_replaces_model_tiers_subtable() {
        let mut config = config_from_toml(&two_provider_config_with_tiers(
            r#"
[model_tiers.small]
provider = "openai"
model = "fast"

[model_tiers.standard]
provider = "openai"
model = "reasoner"

[model_tiers.large]
provider = "anthropic"
model = "opus"
"#,
        ));

        config
            .merge_project_config(partial_from_toml(
                r#"
[model_tiers.small]
provider = "anthropic"
model = "haiku"
"#,
            ))
            .unwrap();

        let small = config.model_tiers.small.unwrap();
        assert_eq!(small.provider, "anthropic");
        assert_eq!(small.model, "haiku");

        let standard = config.model_tiers.standard.unwrap();
        assert_eq!(standard.provider, "openai");
        assert_eq!(standard.model, "reasoner");

        let large = config.model_tiers.large.unwrap();
        assert_eq!(large.provider, "anthropic");
        assert_eq!(large.model, "opus");
    }

    #[test]
    fn user_config_merge_project_config_omits_model_tiers_keeps_user() {
        let mut config = config_from_toml(&two_provider_config_with_tiers(
            r#"
[model_tiers.small]
provider = "openai"
model = "fast"
"#,
        ));

        config
            .merge_project_config(partial_from_toml(
                r#"
language = "English"
"#,
            ))
            .unwrap();

        let small = config.model_tiers.small.unwrap();
        assert_eq!(small.provider, "openai");
        assert_eq!(small.model, "fast");
        assert!(config.model_tiers.standard.is_none());
        assert!(config.model_tiers.large.is_none());
    }

    #[test]
    fn user_config_merge_project_config_adds_model_tiers_when_user_omitted() {
        let mut config = config_from_toml(&two_provider_config_with_tiers(""));

        config
            .merge_project_config(partial_from_toml(
                r#"
[model_tiers.small]
provider = "anthropic"
model = "haiku"

[model_tiers.standard]
provider = "openai"
model = "fast"

[model_tiers.large]
provider = "anthropic"
model = "opus"
"#,
            ))
            .unwrap();

        let small = config.model_tiers.small.unwrap();
        assert_eq!(small.provider, "anthropic");
        assert_eq!(small.model, "haiku");

        let standard = config.model_tiers.standard.unwrap();
        assert_eq!(standard.provider, "openai");
        assert_eq!(standard.model, "fast");

        let large = config.model_tiers.large.unwrap();
        assert_eq!(large.provider, "anthropic");
        assert_eq!(large.model, "opus");
    }

    // 6.3 Resolve 行为

    fn settings_with_tiers(
        active_provider: &str,
        active_model: &str,
        active_effort: Option<ThinkingEffort>,
        model_tiers: ModelTiers,
    ) -> Settings {
        let config = config_from_toml(&two_provider_config_with_tiers(""));
        config
            .to_settings(Some(active_provider), Some(active_model), active_effort)
            .map(|mut s| {
                s.model_tiers = model_tiers;
                s
            })
            .expect("settings should build")
    }

    fn entry(provider: &str, model: &str, effort: Option<ThinkingEffort>) -> ModelTierEntry {
        ModelTierEntry {
            provider: provider.into(),
            model: model.into(),
            thinking_effort: effort,
        }
    }

    #[test]
    fn resolve_tier_returns_configured_when_all_valid() {
        let tiers = ModelTiers {
            small: Some(entry("openai", "reasoner", Some(ThinkingEffort::Low))),
            standard: Some(entry("openai", "fast", None)),
            large: Some(entry("anthropic", "opus", Some(ThinkingEffort::High))),
        };
        let settings = settings_with_tiers("openai", "fast", None, tiers);

        assert_eq!(
            settings.resolve_tier(ModelTier::Small),
            (
                "openai".into(),
                "reasoner".into(),
                Some(ThinkingEffort::Low)
            )
        );
        assert_eq!(
            settings.resolve_tier(ModelTier::Standard),
            ("openai".into(), "fast".into(), None)
        );
        assert_eq!(
            settings.resolve_tier(ModelTier::Large),
            (
                "anthropic".into(),
                "opus".into(),
                Some(ThinkingEffort::High)
            )
        );
    }

    #[test]
    fn resolve_tier_falls_back_when_slot_unset() {
        let settings = settings_with_tiers(
            "openai",
            "reasoner",
            Some(ThinkingEffort::Low),
            ModelTiers::default(),
        );

        for tier in ModelTier::ALL {
            assert_eq!(
                settings.resolve_tier(tier),
                (
                    "openai".into(),
                    "reasoner".into(),
                    Some(ThinkingEffort::Low)
                )
            );
        }
    }

    #[test]
    fn resolve_tier_falls_back_when_provider_missing() {
        let tiers = ModelTiers {
            small: Some(entry("ghost", "fast", None)),
            ..Default::default()
        };
        let settings =
            settings_with_tiers("openai", "reasoner", Some(ThinkingEffort::Medium), tiers);

        assert_eq!(
            settings.resolve_tier(ModelTier::Small),
            (
                "openai".into(),
                "reasoner".into(),
                Some(ThinkingEffort::Medium)
            )
        );
    }

    #[test]
    fn resolve_tier_falls_back_when_model_missing() {
        let tiers = ModelTiers {
            standard: Some(entry("openai", "nonexistent", None)),
            ..Default::default()
        };
        let settings = settings_with_tiers("openai", "reasoner", Some(ThinkingEffort::Low), tiers);

        assert_eq!(
            settings.resolve_tier(ModelTier::Standard),
            (
                "openai".into(),
                "reasoner".into(),
                Some(ThinkingEffort::Low)
            )
        );
    }

    #[test]
    fn resolve_tier_normalizes_thinking_effort_to_none_for_non_thinking_model() {
        let tiers = ModelTiers {
            small: Some(entry("openai", "fast", Some(ThinkingEffort::High))),
            ..Default::default()
        };
        let settings = settings_with_tiers("openai", "reasoner", Some(ThinkingEffort::High), tiers);

        let (provider, model, effort) = settings.resolve_tier(ModelTier::Small);
        assert_eq!(provider, "openai");
        assert_eq!(model, "fast");
        assert_eq!(effort, None);
    }

    #[test]
    fn resolve_tier_normalizes_thinking_effort_to_default_for_thinking_model() {
        let tiers = ModelTiers {
            small: Some(entry("openai", "reasoner", None)),
            ..Default::default()
        };
        let settings = settings_with_tiers("openai", "fast", None, tiers);

        let (provider, model, effort) = settings.resolve_tier(ModelTier::Small);
        assert_eq!(provider, "openai");
        assert_eq!(model, "reasoner");
        assert_eq!(effort, Some(ThinkingEffort::Medium));
    }

    #[test]
    fn resolve_tier_cross_provider_resolution() {
        let tiers = ModelTiers {
            small: Some(entry("anthropic", "opus", None)),
            standard: Some(entry("anthropic", "opus", Some(ThinkingEffort::High))),
            large: Some(entry("openai", "reasoner", Some(ThinkingEffort::Low))),
        };
        let settings = settings_with_tiers("openai", "fast", None, tiers);

        assert_eq!(
            settings.resolve_tier(ModelTier::Small).0,
            "anthropic".to_string()
        );
        assert_eq!(
            settings.resolve_tier(ModelTier::Standard).0,
            "anthropic".to_string()
        );
        assert_eq!(
            settings.resolve_tier(ModelTier::Large).0,
            "openai".to_string()
        );
    }

    #[test]
    fn resolve_tier_does_not_mutate_settings() {
        let tiers = ModelTiers {
            small: Some(entry("openai", "fast", None)),
            standard: Some(entry("openai", "reasoner", Some(ThinkingEffort::High))),
            large: Some(entry("anthropic", "haiku", None)),
        };
        let settings = settings_with_tiers("openai", "fast", Some(ThinkingEffort::Low), tiers);

        let snapshot = settings.clone();
        for _ in 0..3 {
            let _ = settings.resolve_tier(ModelTier::Small);
            let _ = settings.resolve_tier(ModelTier::Standard);
            let _ = settings.resolve_tier(ModelTier::Large);
        }

        assert_eq!(settings.model_tiers, snapshot.model_tiers);
        assert_eq!(settings.thinking_effort, snapshot.thinking_effort);
        assert_eq!(settings.model, snapshot.model);
    }

    // 6.4 端到端加载

    fn temp_root(suffix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "omini-config-{suffix}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ))
    }

    #[test]
    fn load_config_for_cwd_with_model_tiers_passes_through_to_settings() {
        let root_path = temp_root("model-tiers-pass-through");
        let cwd = root_path.join("project");
        fs::create_dir_all(cwd.join(".omini")).unwrap();
        fs::write(
            root_path.join("config.toml"),
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "openai-key"

[providers.openai.models]
fast = {}
reasoner = { thinking = true }

[providers.anthropic]
endpoint = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models]
haiku = {}
opus = { thinking = true }

[model_tiers.small]
provider = "anthropic"
model = "haiku"
thinking_effort = "low"

[model_tiers.standard]
provider = "openai"
model = "fast"

[model_tiers.large]
provider = "anthropic"
model = "opus"
thinking_effort = "high"
"#,
        )
        .unwrap();
        fs::write(
            cwd.join(".omini").join("config.toml"),
            r#"language = "English""#,
        )
        .unwrap();
        let root = OminiRoot::from_path(root_path.clone());

        let config = root.load_config_for_cwd(&cwd).unwrap();
        let settings = config.to_settings(None, None, None).unwrap();

        let small = settings.model_tiers.small.as_ref().unwrap();
        assert_eq!(small.provider, "anthropic");
        assert_eq!(small.model, "haiku");
        assert_eq!(small.thinking_effort, Some(ThinkingEffort::Low));

        let standard = settings.model_tiers.standard.as_ref().unwrap();
        assert_eq!(standard.provider, "openai");
        assert_eq!(standard.model, "fast");
        assert_eq!(standard.thinking_effort, None);

        let large = settings.model_tiers.large.as_ref().unwrap();
        assert_eq!(large.provider, "anthropic");
        assert_eq!(large.model, "opus");
        assert_eq!(large.thinking_effort, Some(ThinkingEffort::High));

        let _ = fs::remove_dir_all(root_path);
    }

    #[test]
    fn load_config_for_cwd_tolerates_invalid_model_tiers_provider() {
        let root_path = temp_root("model-tiers-ghost");
        let cwd = root_path.join("project");
        fs::create_dir_all(cwd.join(".omini")).unwrap();
        fs::write(
            root_path.join("config.toml"),
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "openai-key"

[providers.openai.models]
fast = {}

[model_tiers.small]
provider = "ghost"
model = "phantom"
"#,
        )
        .unwrap();
        let root = OminiRoot::from_path(root_path.clone());

        let config = root.load_config_for_cwd(&cwd).unwrap();
        let settings = config.to_settings(None, None, None).unwrap();

        // 加载不阻断;失效 slot 保留,等 resolve 时才 warn/fallback
        let small = settings.model_tiers.small.as_ref().unwrap();
        assert_eq!(small.provider, "ghost");
        assert_eq!(small.model, "phantom");

        let _ = fs::remove_dir_all(root_path);
    }
}
