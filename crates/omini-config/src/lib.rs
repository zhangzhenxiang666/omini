mod auth;
mod config;
pub mod permissions;
pub mod project;
mod settings;

pub use auth::{AuthEnvironment, AuthStore};
pub use config::{
    AgentConfig, BootstrapProviderConfig, ContextConfig, DEFAULT_CONTEXT_WINDOW,
    EffectiveModelConfig, ModelCapabilities, ModelSelection, ProviderProtocol, RawConfig,
    RawModelConfig, RawProviderConfig, RawRequestOverrides, RawSecretRef, ResolvedConfig,
    ResolvedModel, ResolvedProvider, ResolvedSecret, RoutingConfig, RoutingTier,
    bootstrap_global_config, load_resolved_config_for_cwd,
};
pub use permissions::{PermissionSources, RawBashRulesFile, load_permission_sources};
pub use settings::{
    CompactConfig, ConfigError, McpServerConfig, McpServerTransportConfig, ModelTier,
    ModelTierEntry, ModelTiers, OminiRoot, ProviderProfile, RawPermissionConfig, Settings,
};
