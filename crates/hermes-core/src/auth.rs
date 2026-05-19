use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::platform::{hermes_data_dir, set_file_permissions, set_secure_permissions};

const AUTH_STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AuthStore {
    pub version: u32,
    pub profiles: BTreeMap<String, AuthProfile>,
}

impl Default for AuthStore {
    fn default() -> Self {
        Self {
            version: AUTH_STORE_VERSION,
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AuthProfile {
    pub provider: String,
    pub method: AuthMethod,
    pub base_url: Option<String>,
    pub secret_ref: String,
    pub disabled: bool,
}

impl Default for AuthProfile {
    fn default() -> Self {
        Self {
            provider: String::new(),
            method: AuthMethod::ApiKey,
            base_url: None,
            secret_ref: String::new(),
            disabled: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    ApiKey,
}

impl Default for AuthMethod {
    fn default() -> Self {
        Self::ApiKey
    }
}

impl AuthStore {
    pub fn load_default() -> Result<Self> {
        Self::load_from(default_auth_store_path())
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(path).map_err(|error| {
            Error::Config(format!(
                "Failed to read auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        serde_json::from_str(&raw).map_err(|error| {
            Error::Config(format!(
                "Failed to parse auth store '{}': {}",
                path.display(),
                error
            ))
        })
    }

    pub fn save_default(&self) -> Result<()> {
        self.save_to(default_auth_store_path())
    }

    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::Config(format!(
                    "Failed to create auth store directory '{}': {}",
                    parent.display(),
                    error
                ))
            })?;
            set_secure_permissions(parent).map_err(|error| {
                Error::Config(format!(
                    "Failed to secure auth store directory '{}': {}",
                    parent.display(),
                    error
                ))
            })?;
        }

        let raw = serde_json::to_string_pretty(self)?;
        fs::write(path, raw).map_err(|error| {
            Error::Config(format!(
                "Failed to write auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        set_file_permissions(path, 0o600).map_err(|error| {
            Error::Config(format!(
                "Failed to secure auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        Ok(())
    }

    pub fn upsert_api_key_env_profile(
        &mut self,
        name: impl Into<String>,
        provider: impl Into<String>,
        env_var: impl Into<String>,
        base_url: Option<String>,
    ) -> Result<()> {
        let name = validate_name("auth profile", name.into())?;
        let provider = validate_name("provider", provider.into())?;
        let env_var = validate_env_var(env_var.into())?;
        let base_url = validate_base_url(base_url)?;

        self.profiles.insert(
            name,
            AuthProfile {
                provider,
                method: AuthMethod::ApiKey,
                base_url,
                secret_ref: format!("env:{}", env_var),
                disabled: false,
            },
        );
        Ok(())
    }

    pub fn remove_profile(&mut self, name: &str) -> bool {
        self.profiles.remove(name).is_some()
    }

    pub fn resolve_api_key(&self, name: &str) -> Result<String> {
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| Error::MissingConfig {
                key: format!("auth profile '{}'", name),
            })?;
        profile.resolve_api_key()
    }
}

impl AuthProfile {
    pub fn resolved_env_var(&self) -> Option<&str> {
        self.secret_ref.strip_prefix("env:")
    }

    pub fn resolve_api_key(&self) -> Result<String> {
        if self.disabled {
            return Err(Error::Config(format!(
                "Auth profile '{}' is disabled",
                self.provider
            )));
        }
        if self.method != AuthMethod::ApiKey {
            return Err(Error::Config(format!(
                "Unsupported auth method for provider '{}'",
                self.provider
            )));
        }

        let env_var = self.resolved_env_var().ok_or_else(|| {
            Error::Config(format!(
                "Auth profile '{}' uses an unsupported secret reference",
                self.provider
            ))
        })?;

        env::var(env_var)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::MissingConfig {
                key: format!("environment variable {} for auth profile", env_var),
            })
    }
}

pub fn default_auth_store_path() -> PathBuf {
    hermes_data_dir().join("auth.json")
}

fn validate_name(label: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::Config(format!("Invalid {} name", label)));
    }
    Ok(value)
}

fn validate_env_var(value: String) -> Result<String> {
    let value = value.trim().to_string();
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(Error::Config(
            "Invalid environment variable name".to_string(),
        ));
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(Error::Config(
            "Invalid environment variable name".to_string(),
        ));
    }
    if chars.any(|ch| !(ch == '_' || ch.is_ascii_alphanumeric())) {
        return Err(Error::Config(
            "Invalid environment variable name".to_string(),
        ));
    }
    Ok(value)
}

fn validate_base_url(value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().any(char::is_control) {
        return Err(Error::Config("Invalid auth profile base URL".to_string()));
    }
    reqwest::Url::parse(&value).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_auth_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hermes_auth_{}_{}.json", name, std::process::id()))
    }

    #[test]
    fn auth_store_round_trips_without_secret_values() {
        let path = temp_auth_path("round_trip");
        let _ = fs::remove_file(&path);
        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile(
                "openai-default",
                "openai",
                "OPENAI_API_KEY",
                Some("https://api.openai.com/v1".to_string()),
            )
            .unwrap();

        store.save_to(&path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("env:OPENAI_API_KEY"));
        assert!(!raw.contains("sk-"));

        let loaded = AuthStore::load_from(&path).unwrap();
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(
            loaded.profiles["openai-default"].resolved_env_var(),
            Some("OPENAI_API_KEY")
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn invalid_env_var_names_are_rejected() {
        let mut store = AuthStore::default();

        let result = store.upsert_api_key_env_profile("bad", "openai", "OPENAI API KEY", None);

        assert!(result.is_err());
    }

    #[test]
    fn missing_env_secret_returns_missing_config() {
        let mut store = AuthStore::default();
        let missing_env = format!("HERMES_TEST_MISSING_API_KEY_{}", std::process::id());
        store
            .upsert_api_key_env_profile("missing-secret", "openai", missing_env, None)
            .unwrap();

        let result = store.resolve_api_key("missing-secret");

        assert!(matches!(result, Err(Error::MissingConfig { .. })));
    }

    #[test]
    fn base_url_rejects_control_characters() {
        let mut store = AuthStore::default();

        let result = store.upsert_api_key_env_profile(
            "bad-url",
            "openai",
            "OPENAI_API_KEY",
            Some("https://api.openai.com/v1\nspoof".to_string()),
        );

        assert!(result.is_err());
    }

    #[test]
    fn remove_profile_reports_whether_profile_existed() {
        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile("openai-default", "openai", "OPENAI_API_KEY", None)
            .unwrap();

        assert!(store.remove_profile("openai-default"));
        assert!(!store.remove_profile("openai-default"));
    }
}
