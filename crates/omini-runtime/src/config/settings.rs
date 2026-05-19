use crate::permissions::RawPermissionConfig;
pub use crate::types::config::{
    ConfigError, ModelConfig, ProviderProfile, ProviderType, Settings, ThinkingEffort,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

/// 用户配置文件顶层结构，对应 ~/.omini/config.toml
#[derive(Debug, Clone, Deserialize)]
pub struct UserConfig {
    /// 供应商配置表，key 为供应商名称
    pub providers: HashMap<String, ProviderConfig>,
    /// Optional user-facing response language preference.
    pub language: Option<String>,
    /// Optional tool permission rules.
    pub permissions: Option<RawPermissionConfig>,
}

/// 单个供应商配置（用户配置文件中）
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// 展示名称（可选，用于 TUI；不填则回退到 TOML 表 key）
    pub name: Option<String>,
    /// 供应商类型（如 "OpenAI" / "Anthropic"）
    pub endpoint: ProviderType,
    /// API 基础地址（必填）
    pub base_url: String,
    /// API 密钥（必填）
    pub api_key: String,
    /// 模型配置表，key 为模型 ID，value 为可选的展示名称/限制/思考开关
    pub models: Option<HashMap<String, ModelEntry>>,
}

/// 用户配置中的模型条目（TOML 内联表）。
///
/// 从 `~/.omini/config.toml` 中读取时 key 即为模型 ID，
/// 因此这里不再包含 `id` 字段，`limit` 和 `thinking` 为可选（由默认值填充）。
#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    /// 展示名称（可选，用于 TUI）
    pub name: Option<String>,
    /// 上下文长度限制（可选，默认 256k）
    pub limit: Option<u32>,
    /// 是否支持思考模式（可选，默认关闭）
    pub thinking: Option<bool>,
}

impl UserConfig {
    /// 将用户配置转换为内部运行时 `Settings`，并应用项目状态中的偏好。
    ///
    /// - `active_provider` / `active_model` 来自 `ProjectState`，传入 `None` 则回退到第一个。
    pub fn to_settings(
        &self,
        active_provider: Option<&str>,
        active_model: Option<&str>,
        thinking_effort: Option<ThinkingEffort>,
    ) -> Result<Settings, ConfigError> {
        // 确定活跃 provider：优先取传入的，否则取第一个
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
                        .map(|(id, entry)| ModelConfig {
                            id: id.clone(),
                            name: entry.name.clone(),
                            limit: entry.limit.unwrap_or(256000),
                            thinking: entry.thinking.unwrap_or(false),
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

        // 确定活跃模型：优先取传入的，否则取第一个
        let model = active_model
            .filter(|m| providers[active_name].models.iter().any(|mc| mc.id == *m))
            .unwrap_or(&first_model)
            .to_string();

        Ok(Settings {
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
        })
    }

    /// 验证配置是否合法：
    /// - 至少有一个 provider
    /// - 每个 provider 至少有一个 model（不能为空表）
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

/// 表示 `~/.omini/` 用户数据根目录。
pub struct OminiRoot {
    path: PathBuf,
}

impl OminiRoot {
    /// 通过 `dirs` 查找用户家目录，拼接 `.omini` 并创建（如果不存在）。
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

    /// 返回 `~/.omini/` 目录路径。
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 返回 `~/.omini/config.toml` 路径。
    pub fn config_path(&self) -> PathBuf {
        self.path.join("config.toml")
    }

    /// 返回 `~/.omini/omini.db` 路径。
    pub fn db_path(&self) -> PathBuf {
        self.path.join("omini.db")
    }

    /// 加载并解析 `~/.omini/config.toml`，返回 `UserConfig`。
    pub fn load_config(&self) -> Result<UserConfig, ConfigError> {
        let path = self.config_path();
        let path_str = path.display().to_string();
        let content = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::ConfigLoad { path, source: e })?;
        let config: UserConfig =
            toml::from_str(&content).map_err(|e| ConfigError::ConfigParse {
                path: path_str,
                source: e,
            })?;
        Ok(config)
    }

    /// 返回 `~/.omini/projects/` 目录操作句柄。
    pub fn projects_dir(&self) -> super::project::ProjectsDir {
        super::project::ProjectsDir::new(&self.path)
    }

    /// 初始化当前工作目录对应的项目目录。
    ///
    /// 如果 `~/.omini/projects/<sanitized-cwd>/` 尚不存在则自动创建，
    /// 并写入含 `created_at` 的 `state.toml`，默认使用第一个 provider / model。
    /// 在创建运行时之前调用一次即可。
    pub fn init_project(
        &self,
        cwd: &Path,
        config: &UserConfig,
    ) -> Result<super::project::ProjectDir, ConfigError> {
        self.projects_dir().for_cwd(cwd, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_from_toml(content: &str) -> UserConfig {
        toml::from_str(content).expect("config should parse")
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
}
