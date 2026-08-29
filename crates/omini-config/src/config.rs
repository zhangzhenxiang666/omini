use crate::{
    AuthEnvironment, CompactConfig, ConfigError, McpServerConfig, ModelTierEntry, ModelTiers,
    OminiRoot, ProviderProfile, RawPermissionConfig, Settings,
};
use indexmap::IndexMap;
use omini_domain::config::{
    InputModality, ModelInfo, ProviderEndpointKind, ProviderInfo, ThinkingEffort,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::Table;
use toml_edit::value;
use url::Url;

pub const DEFAULT_CONTEXT_WINDOW: u32 = 256_000;

pub type ProviderProtocol = ProviderEndpointKind;

/// 首次引导写入的最小 provider/model 信息。认证材料由 auth.json 单独管理。
#[derive(Debug, Clone)]
pub struct BootstrapProviderConfig {
    pub provider_id: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub model_id: String,
    pub api_key_env: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedSecret(String);

impl ResolvedSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResolvedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolvedSecret(REDACTED)")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawSecretRef {
    Literal(String),
    Env { env: String },
}

impl RawSecretRef {
    fn resolve(&self, auth_environment: &AuthEnvironment) -> Result<ResolvedSecret, ConfigError> {
        match self {
            Self::Literal(value) => Ok(ResolvedSecret(value.clone())),
            Self::Env { env } => std::env::var(env)
                .ok()
                .or_else(|| auth_environment.get(env).map(str::to_string))
                .map(ResolvedSecret)
                .ok_or_else(|| ConfigError::MissingEnv(env.clone())),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRequestOverrides {
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: Map<String, Value>,
}

impl RawRequestOverrides {
    fn merge_into(self, target: &mut Self) {
        target.headers.extend(self.headers);
        target.body.extend(self.body);
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawModelConfig {
    pub name: Option<String>,
    pub context_window: Option<u32>,
    pub thinking: Option<bool>,
    pub input: Option<Vec<InputModality>>,
    pub request: Option<RawRequestOverrides>,
}

impl RawModelConfig {
    fn merge_into(self, target: &mut Self) {
        if self.name.is_some() {
            target.name = self.name;
        }
        if self.context_window.is_some() {
            target.context_window = self.context_window;
        }
        if self.thinking.is_some() {
            target.thinking = self.thinking;
        }
        if self.input.is_some() {
            target.input = self.input;
        }
        if let Some(request) = self.request {
            request.merge_into(target.request.get_or_insert_with(Default::default));
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProviderConfig {
    pub name: Option<String>,
    pub protocol: Option<ProviderProtocol>,
    pub base_url: Option<String>,
    pub api_key: Option<RawSecretRef>,
    pub request: Option<RawRequestOverrides>,
    #[serde(default)]
    pub models: IndexMap<String, RawModelConfig>,
}

impl RawProviderConfig {
    fn merge_into(self, target: &mut Self) {
        if self.name.is_some() {
            target.name = self.name;
        }
        if self.protocol.is_some() {
            target.protocol = self.protocol;
        }
        if self.base_url.is_some() {
            target.base_url = self.base_url;
        }
        if self.api_key.is_some() {
            target.api_key = self.api_key;
        }
        if let Some(request) = self.request {
            request.merge_into(target.request.get_or_insert_with(Default::default));
        }
        for (id, model) in self.models {
            match target.models.get_mut(&id) {
                Some(existing) => model.merge_into(existing),
                None => {
                    target.models.insert(id, model);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawAgentConfig {
    pub language: Option<String>,
}

impl RawAgentConfig {
    fn merge_into(self, target: &mut Self) {
        if self.language.is_some() {
            target.language = self.language;
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCompactConfig {
    pub enabled: Option<bool>,
    pub preserve_recent: Option<usize>,
    pub buffer_tokens: Option<usize>,
    pub summary_output_tokens: Option<usize>,
    pub max_consecutive_failures: Option<usize>,
}

impl RawCompactConfig {
    fn merge_into(self, target: &mut Self) {
        if self.enabled.is_some() {
            target.enabled = self.enabled;
        }
        if self.preserve_recent.is_some() {
            target.preserve_recent = self.preserve_recent;
        }
        if self.buffer_tokens.is_some() {
            target.buffer_tokens = self.buffer_tokens;
        }
        if self.summary_output_tokens.is_some() {
            target.summary_output_tokens = self.summary_output_tokens;
        }
        if self.max_consecutive_failures.is_some() {
            target.max_consecutive_failures = self.max_consecutive_failures;
        }
    }

    fn resolve(self) -> CompactConfig {
        let defaults = CompactConfig::default();
        CompactConfig {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            preserve_recent: self.preserve_recent.unwrap_or(defaults.preserve_recent),
            buffer_tokens: self.buffer_tokens.unwrap_or(defaults.buffer_tokens),
            summary_output_tokens: self
                .summary_output_tokens
                .unwrap_or(defaults.summary_output_tokens),
            max_consecutive_failures: self
                .max_consecutive_failures
                .unwrap_or(defaults.max_consecutive_failures),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawContextConfig {
    pub compaction: Option<RawCompactConfig>,
}

impl RawContextConfig {
    fn merge_into(self, target: &mut Self) {
        if let Some(compaction) = self.compaction {
            compaction.merge_into(target.compaction.get_or_insert_with(Default::default));
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPermissionOverlay {
    pub allow: Option<Vec<String>>,
    pub ask: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}

impl RawPermissionOverlay {
    fn merge_into(self, target: &mut Self) {
        if self.allow.is_some() {
            target.allow = self.allow;
        }
        if self.ask.is_some() {
            target.ask = self.ask;
        }
        if self.deny.is_some() {
            target.deny = self.deny;
        }
    }

    fn resolve(self) -> RawPermissionConfig {
        RawPermissionConfig {
            allow: self.allow.unwrap_or_default(),
            ask: self.ask.unwrap_or_default(),
            deny: self.deny.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawModelSelection {
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<ThinkingEffort>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRoutingConfig {
    pub small: Option<RawModelSelection>,
    pub standard: Option<RawModelSelection>,
    pub large: Option<RawModelSelection>,
}

impl RawRoutingConfig {
    fn merge_into(self, target: &mut Self) {
        if self.small.is_some() {
            target.small = self.small;
        }
        if self.standard.is_some() {
            target.standard = self.standard;
        }
        if self.large.is_some() {
            target.large = self.large;
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub agent: Option<RawAgentConfig>,
    #[serde(default)]
    pub providers: IndexMap<String, RawProviderConfig>,
    pub context: Option<RawContextConfig>,
    #[serde(default)]
    pub mcp: IndexMap<String, McpServerConfig>,
    pub permissions: Option<RawPermissionOverlay>,
    pub routing: Option<RawRoutingConfig>,
}

impl RawConfig {
    pub fn merge_project_config(&mut self, project: Self) -> Result<(), ConfigError> {
        if let Some(agent) = project.agent {
            agent.merge_into(self.agent.get_or_insert_with(Default::default));
        }
        for (id, provider) in project.providers {
            match self.providers.get_mut(&id) {
                Some(existing) => provider.merge_into(existing),
                None => {
                    self.providers.insert(id, provider);
                }
            }
        }
        if let Some(context) = project.context {
            context.merge_into(self.context.get_or_insert_with(Default::default));
        }
        self.mcp.extend(project.mcp);
        if let Some(permissions) = project.permissions {
            permissions.merge_into(self.permissions.get_or_insert_with(Default::default));
        }
        if let Some(routing) = project.routing {
            routing.merge_into(self.routing.get_or_insert_with(Default::default));
        }
        Ok(())
    }

    pub fn resolve(self) -> Result<ResolvedConfig, ConfigError> {
        self.resolve_with_auth(&AuthEnvironment::default())
    }

    pub fn resolve_with_auth(
        self,
        auth_environment: &AuthEnvironment,
    ) -> Result<ResolvedConfig, ConfigError> {
        if self.providers.is_empty() {
            return Err(ConfigError::NoActiveProvider);
        }

        let mut providers = IndexMap::new();
        for (id, provider) in self.providers {
            if id.trim().is_empty() {
                return Err(ConfigError::InvalidProviderId);
            }
            let protocol = provider
                .protocol
                .ok_or_else(|| ConfigError::MissingProviderField {
                    provider: id.clone(),
                    field: "protocol",
                })?;
            let base_url = provider
                .base_url
                .ok_or_else(|| ConfigError::MissingProviderField {
                    provider: id.clone(),
                    field: "base_url",
                })?;
            let url = Url::parse(&base_url).map_err(|source| ConfigError::InvalidBaseUrl {
                provider: id.clone(),
                source,
            })?;
            if provider.models.is_empty() {
                return Err(ConfigError::NoModels(id));
            }
            let mut models = IndexMap::new();
            for (model_id, model) in provider.models {
                if model_id.trim().is_empty() {
                    return Err(ConfigError::InvalidModelId {
                        provider: id.clone(),
                    });
                }
                let context_window = model.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
                if context_window == 0 {
                    return Err(ConfigError::InvalidContextWindow {
                        provider: id.clone(),
                        model: model_id,
                    });
                }
                models.insert(
                    model_id.clone(),
                    ResolvedModel {
                        id: model_id.clone(),
                        name: model.name.unwrap_or(model_id),
                        context_window,
                        capabilities: ModelCapabilities {
                            thinking: model.thinking.unwrap_or(false),
                            input: model.input.unwrap_or_default(),
                        },
                        request: model.request.unwrap_or_default(),
                    },
                );
            }
            providers.insert(
                id.clone(),
                ResolvedProvider {
                    id: id.clone(),
                    name: provider.name.unwrap_or(id),
                    protocol,
                    base_url: url,
                    api_key: provider
                        .api_key
                        .map(|secret| secret.resolve(auth_environment))
                        .transpose()?,
                    request: provider.request.unwrap_or_default(),
                    models,
                },
            );
        }

        let routing = resolve_routing(self.routing.unwrap_or_default(), &providers)?;
        Ok(ResolvedConfig {
            agent: AgentConfig {
                language: self.agent.and_then(|agent| agent.language),
            },
            providers,
            context: ContextConfig {
                compaction: self
                    .context
                    .and_then(|context| context.compaction)
                    .unwrap_or_default()
                    .resolve(),
            },
            mcp: self.mcp,
            permissions: self.permissions.map(RawPermissionOverlay::resolve),
            routing,
        })
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextConfig {
    pub compaction: CompactConfig,
}

#[derive(Debug, Clone)]
pub struct ModelCapabilities {
    pub thinking: bool,
    pub input: Vec<InputModality>,
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub id: String,
    pub name: String,
    pub context_window: u32,
    pub capabilities: ModelCapabilities,
    pub request: RawRequestOverrides,
}

#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub id: String,
    pub name: String,
    pub protocol: ProviderProtocol,
    pub base_url: Url,
    pub api_key: Option<ResolvedSecret>,
    pub request: RawRequestOverrides,
    pub models: IndexMap<String, ResolvedModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    pub active_provider: String,
    pub model: String,
    pub thinking_effort: Option<ThinkingEffort>,
}

#[derive(Debug, Clone)]
pub struct EffectiveModelConfig {
    pub provider_id: String,
    pub model_id: String,
    pub protocol: ProviderProtocol,
    pub base_url: Url,
    pub api_key: Option<ResolvedSecret>,
    pub context_window: u32,
    pub capabilities: ModelCapabilities,
    pub headers: HashMap<String, String>,
    pub body: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct RoutingConfig {
    pub small: Option<ModelSelection>,
    pub standard: Option<ModelSelection>,
    pub large: Option<ModelSelection>,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub agent: AgentConfig,
    pub providers: IndexMap<String, ResolvedProvider>,
    pub context: ContextConfig,
    pub mcp: IndexMap<String, McpServerConfig>,
    pub permissions: Option<RawPermissionConfig>,
    pub routing: RoutingConfig,
}

impl ResolvedConfig {
    pub fn first_selection(&self) -> ModelSelection {
        let (provider_id, provider) = self.providers.first().expect("validated providers");
        let (model_id, _) = provider.models.first().expect("validated models");
        ModelSelection {
            active_provider: provider_id.clone(),
            model: model_id.clone(),
            thinking_effort: None,
        }
    }

    pub fn to_settings(
        &self,
        active_provider: Option<&str>,
        active_model: Option<&str>,
        thinking_effort: Option<ThinkingEffort>,
    ) -> Result<Settings, ConfigError> {
        let requested = active_provider
            .and_then(|provider| {
                self.providers.get(provider).map(|profile| ModelSelection {
                    active_provider: provider.to_string(),
                    model: active_model
                        .filter(|model| profile.models.contains_key(*model))
                        .map(str::to_string)
                        .or_else(|| profile.models.first().map(|(id, _)| id.clone()))
                        .expect("validated provider has models"),
                    thinking_effort,
                })
            })
            .unwrap_or_else(|| self.first_selection());
        let selection = self
            .normalize_selection(requested)
            .or_else(|error| match error {
                ConfigError::UnknownProvider(_) | ConfigError::UnknownModel { .. } => {
                    self.normalize_selection(self.first_selection())
                }
                other => Err(other),
            })?;
        let effective = self.effective_model_config(&selection)?;
        let providers = self
            .providers
            .iter()
            .map(|(id, provider)| {
                (
                    id.clone(),
                    ProviderProfile {
                        name: provider.name.clone(),
                        endpoint: provider.protocol,
                        api_key: provider
                            .api_key
                            .as_ref()
                            .map(|secret| secret.expose().to_string())
                            .unwrap_or_default(),
                        base_url: provider.base_url.clone(),
                        models: provider
                            .models
                            .values()
                            .map(|model| {
                                let mut headers = provider.request.headers.clone();
                                headers.extend(model.request.headers.clone());
                                let mut body = provider.request.body.clone();
                                body.extend(model.request.body.clone());
                                ModelInfo {
                                    id: model.id.clone(),
                                    name: Some(model.name.clone()),
                                    limit: model.context_window,
                                    thinking: model.capabilities.thinking,
                                    input_modalities: (!model.capabilities.input.is_empty())
                                        .then(|| model.capabilities.input.clone()),
                                    extra_headers: (!headers.is_empty()).then_some(headers),
                                    extra_body: (!body.is_empty()).then_some(body),
                                }
                            })
                            .collect(),
                    },
                )
            })
            .collect();
        Ok(Settings {
            api_key: effective
                .api_key
                .as_ref()
                .map(|secret| secret.expose().to_string())
                .unwrap_or_default(),
            base_url: effective.base_url,
            model: selection.model,
            endpoint: effective.protocol,
            providers,
            active_provider: selection.active_provider,
            system_prompt: None,
            language: self.agent.language.clone(),
            max_turns: None,
            cwd: std::env::current_dir()?,
            thinking_effort: selection.thinking_effort,
            permissions: self.permissions.clone(),
            compact: self.context.compaction.clone(),
            mcp_servers: self
                .mcp
                .iter()
                .map(|(id, server)| (id.clone(), server.clone()))
                .collect(),
            model_tiers: ModelTiers {
                small: self.routing.small.as_ref().map(model_tier_entry),
                standard: self.routing.standard.as_ref().map(model_tier_entry),
                large: self.routing.large.as_ref().map(model_tier_entry),
            },
        })
    }

    pub fn normalize_selection(
        &self,
        mut selection: ModelSelection,
    ) -> Result<ModelSelection, ConfigError> {
        let model = self.model(&selection.active_provider, &selection.model)?;
        if !model.capabilities.thinking {
            selection.thinking_effort = None;
        } else if selection.thinking_effort.is_none() {
            selection.thinking_effort = Some(ThinkingEffort::default());
        }
        Ok(selection)
    }

    pub fn model(&self, provider_id: &str, model_id: &str) -> Result<&ResolvedModel, ConfigError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| ConfigError::UnknownProvider(provider_id.to_string()))?;
        provider
            .models
            .get(model_id)
            .ok_or_else(|| ConfigError::UnknownModel {
                provider: provider_id.to_string(),
                model: model_id.to_string(),
            })
    }

    pub fn effective_model_config(
        &self,
        selection: &ModelSelection,
    ) -> Result<EffectiveModelConfig, ConfigError> {
        let provider = self
            .providers
            .get(&selection.active_provider)
            .ok_or_else(|| ConfigError::UnknownProvider(selection.active_provider.clone()))?;
        let model =
            provider
                .models
                .get(&selection.model)
                .ok_or_else(|| ConfigError::UnknownModel {
                    provider: selection.active_provider.clone(),
                    model: selection.model.clone(),
                })?;
        let mut headers = provider.request.headers.clone();
        headers.extend(model.request.headers.clone());
        let mut body = provider.request.body.clone();
        body.extend(model.request.body.clone());
        Ok(EffectiveModelConfig {
            provider_id: provider.id.clone(),
            model_id: model.id.clone(),
            protocol: provider.protocol,
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            context_window: model.context_window,
            capabilities: model.capabilities.clone(),
            headers,
            body,
        })
    }

    pub fn catalog(&self) -> Vec<ProviderInfo> {
        self.providers
            .values()
            .map(|provider| ProviderInfo {
                id: provider.id.clone(),
                name: provider.name.clone(),
                endpoint: provider.protocol,
                base_url: provider.base_url.to_string(),
                models: provider
                    .models
                    .values()
                    .map(|model| ModelInfo {
                        id: model.id.clone(),
                        name: Some(model.name.clone()),
                        limit: model.context_window,
                        thinking: model.capabilities.thinking,
                        input_modalities: (!model.capabilities.input.is_empty())
                            .then(|| model.capabilities.input.clone()),
                        extra_headers: None,
                        extra_body: None,
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn routing_selection(
        &self,
        tier: RoutingTier,
        fallback: &ModelSelection,
    ) -> ModelSelection {
        match tier {
            RoutingTier::Small => self.routing.small.as_ref(),
            RoutingTier::Standard => self.routing.standard.as_ref(),
            RoutingTier::Large => self.routing.large.as_ref(),
        }
        .cloned()
        .unwrap_or_else(|| fallback.clone())
    }
}

fn model_tier_entry(selection: &ModelSelection) -> ModelTierEntry {
    ModelTierEntry {
        provider: selection.active_provider.clone(),
        model: selection.model.clone(),
        thinking_effort: selection.thinking_effort,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingTier {
    Small,
    Standard,
    Large,
}

fn resolve_routing(
    raw: RawRoutingConfig,
    providers: &IndexMap<String, ResolvedProvider>,
) -> Result<RoutingConfig, ConfigError> {
    fn resolve(
        selection: Option<RawModelSelection>,
        providers: &IndexMap<String, ResolvedProvider>,
    ) -> Result<Option<ModelSelection>, ConfigError> {
        selection
            .map(|selection| {
                let provider = providers.get(&selection.provider).ok_or_else(|| {
                    ConfigError::UnknownRoutingProvider(selection.provider.clone())
                })?;
                let model = provider.models.get(&selection.model).ok_or_else(|| {
                    ConfigError::UnknownRoutingModel {
                        provider: selection.provider.clone(),
                        model: selection.model.clone(),
                    }
                })?;
                let thinking_effort = if model.capabilities.thinking {
                    selection
                        .thinking_effort
                        .or(Some(ThinkingEffort::default()))
                } else {
                    None
                };
                Ok(ModelSelection {
                    active_provider: selection.provider,
                    model: selection.model,
                    thinking_effort,
                })
            })
            .transpose()
    }

    Ok(RoutingConfig {
        small: resolve(raw.small, providers)?,
        standard: resolve(raw.standard, providers)?,
        large: resolve(raw.large, providers)?,
    })
}

pub fn load_resolved_config_for_cwd(
    root: &OminiRoot,
    cwd: &Path,
) -> Result<ResolvedConfig, ConfigError> {
    let global_path = root.config_path();
    let mut config: RawConfig = match load_toml_file(&global_path) {
        Ok(config) => config,
        Err(ConfigError::ConfigLoad { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            RawConfig::default()
        }
        Err(error) => return Err(error),
    };
    let project_path = root.project_config_path(cwd);
    if project_path.exists() {
        let project: RawConfig = load_toml_file(&project_path)?;
        config.merge_project_config(project)?;
    }
    config.resolve_with_auth(&root.load_auth_environment()?)
}

/// 保留用户已有 TOML 的无关段与注释，仅补齐首次引导指定的 provider/model。
pub fn bootstrap_global_config(
    root: &OminiRoot,
    bootstrap: &BootstrapProviderConfig,
) -> Result<(), ConfigError> {
    if bootstrap.provider_id.trim().is_empty() {
        return Err(ConfigError::InvalidProviderId);
    }
    if bootstrap.model_id.trim().is_empty() {
        return Err(ConfigError::InvalidModelId {
            provider: bootstrap.provider_id.clone(),
        });
    }
    let path = root.config_path();
    let content = if path.exists() {
        std::fs::read_to_string(&path).map_err(|source| ConfigError::ConfigLoad {
            path: path.clone(),
            source,
        })?
    } else {
        String::new()
    };
    let mut document =
        content
            .parse::<DocumentMut>()
            .map_err(|source| ConfigError::ConfigEdit {
                path: path.clone(),
                source,
            })?;
    let protocol = match bootstrap.protocol {
        ProviderProtocol::OpenAI => "openai",
        ProviderProtocol::Anthropic => "anthropic",
    };
    let providers = ensure_table(&mut document["providers"])?;
    let provider = ensure_table(&mut providers[&bootstrap.provider_id])?;
    provider["protocol"] = value(protocol);
    provider["base_url"] = value(&bootstrap.base_url);
    let models = ensure_table(&mut provider["models"])?;
    let model = &mut models[&bootstrap.model_id];
    if model.is_none() {
        *model = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if let Some(env) = &bootstrap.api_key_env {
        let mut secret = toml_edit::InlineTable::new();
        secret.insert("env", toml_edit::Value::from(env.as_str()));
        provider["api_key"] = toml_edit::Item::Value(toml_edit::Value::InlineTable(secret));
    }
    write_atomic(&path, document.to_string().as_bytes())
}

fn ensure_table(item: &mut Item) -> Result<&mut Table, ConfigError> {
    if item.is_none() {
        *item = Item::Table(Table::new());
    }
    item.as_table_mut()
        .ok_or_else(|| ConfigError::BootstrapTableConflict("providers".to_string()))
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<(), ConfigError> {
    let parent = path.parent().ok_or_else(|| ConfigError::ConfigLoad {
        path: PathBuf::from(path),
        source: std::io::Error::other("config path has no parent"),
    })?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    use std::io::Write;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn load_toml_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|source| ConfigError::ConfigLoad {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&content).map_err(|source| ConfigError::ConfigParse {
        path: path.display().to_string(),
        source,
    })
}
