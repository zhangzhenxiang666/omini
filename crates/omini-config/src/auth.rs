use crate::ConfigError;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// 用户级认证仓库。配置只保存可供 provider 引用的环境变量值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthStore {
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct AuthEnvironment {
    values: BTreeMap<String, String>,
}

impl AuthEnvironment {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
}

impl AuthStore {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path).map_err(|source| ConfigError::AuthLoad {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&content).map_err(|source| ConfigError::AuthParse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn environment(&self) -> AuthEnvironment {
        AuthEnvironment {
            values: self.env.clone(),
        }
    }

    pub fn upsert_env(&mut self, variable: String, value: String) -> Result<(), ConfigError> {
        if variable.trim().is_empty() {
            return Err(ConfigError::InvalidAuthEnvironmentVariable);
        }
        self.env.insert(variable, value);
        Ok(())
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), ConfigError> {
        let parent = path
            .parent()
            .ok_or_else(|| ConfigError::InvalidAuthPath(path.to_path_buf()))?;
        fs::create_dir_all(parent)?;
        let temp = path.with_extension(format!("tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(self)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        set_private_permissions(&temp)?;
        fs::rename(&temp, path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_store_serializes_as_a_flat_environment_map() {
        let store = AuthStore {
            env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "secret".to_string())]),
        };

        assert_eq!(
            serde_json::to_value(store).unwrap(),
            serde_json::json!({ "env": { "OPENAI_API_KEY": "secret" } })
        );
    }
}
