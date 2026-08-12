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
    /// 可选模型分级配置;缺失 = 所有 tier fallback 当前线程模型。
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
        //   3) 同时存在项目 .omini/permissions.toml 时的合并顺序 + diagnostic。
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
