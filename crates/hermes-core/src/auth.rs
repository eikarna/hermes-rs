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
    BearerToken,
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
        let store: Self = serde_json::from_str(&raw).map_err(|error| {
            Error::Config(format!(
                "Failed to parse auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        store.validate()?;
        Ok(store)
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

    pub fn upsert_bearer_token_env_profile(
        &mut self,
        name: impl Into<String>,
        provider: impl Into<String>,
        env_var: impl Into<String>,
        base_url: Option<String>,
    ) -> Result<()> {
        let name = validate_name("auth profile", name.into())?;
        let provider = validate_name("provider", provider.into())?;
        let env_var = validate_env_var(env_var.into())?;
        let base_url = validate_base_url(base_url)?.ok_or_else(|| {
            Error::Config("Bearer token auth profiles require a base URL".to_string())
        })?;

        self.profiles.insert(
            name,
            AuthProfile {
                provider,
                method: AuthMethod::BearerToken,
                base_url: Some(base_url),
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

    pub fn resolve_auth_token(&self, name: &str) -> Result<String> {
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| Error::MissingConfig {
                key: format!("auth profile '{}'", name),
            })?;
        profile.resolve_auth_token()
    }

    fn validate(&self) -> Result<()> {
        if self.version != AUTH_STORE_VERSION {
            return Err(Error::Config(format!(
                "Unsupported auth store version: {}",
                self.version
            )));
        }
        for (name, profile) in &self.profiles {
            validate_name("auth profile", name.clone())?;
            profile.validate()?;
        }
        Ok(())
    }
}

impl AuthProfile {
    pub fn resolved_env_var(&self) -> Option<&str> {
        self.secret_ref.strip_prefix("env:")
    }

    pub fn resolve_api_key(&self) -> Result<String> {
        if self.method != AuthMethod::ApiKey {
            return Err(Error::Config(format!(
                "Auth profile '{}' is not an API key profile",
                self.provider
            )));
        }
        self.resolve_auth_token()
    }

    pub fn resolve_auth_token(&self) -> Result<String> {
        self.validate()?;
        if self.disabled {
            return Err(Error::Config(format!(
                "Auth profile '{}' is disabled",
                self.provider
            )));
        }
        match self.method {
            AuthMethod::ApiKey | AuthMethod::BearerToken => {}
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

    fn validate(&self) -> Result<()> {
        validate_name("provider", self.provider.clone())?;
        if self.resolved_env_var().is_none() {
            return Err(Error::Config(format!(
                "Auth profile '{}' uses an unsupported secret reference",
                self.provider
            )));
        }
        if self.method == AuthMethod::BearerToken && self.base_url.is_none() {
            return Err(Error::Config(format!(
                "Bearer auth profile '{}' requires a base URL",
                self.provider
            )));
        }
        let _ = validate_base_url(self.base_url.clone())?;
        Ok(())
    }
}

pub fn default_auth_store_path() -> PathBuf {
    if let Ok(path) = env::var("HERMES_AUTH_STORE") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
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
    fn bearer_token_profile_round_trips_without_secret_values() {
        let path = temp_auth_path("bearer_round_trip");
        let _ = fs::remove_file(&path);
        let mut store = AuthStore::default();
        store
            .upsert_bearer_token_env_profile(
                "google-default",
                "google-gemini",
                "GOOGLE_OAUTH_ACCESS_TOKEN",
                Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            )
            .unwrap();

        store.save_to(&path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("bearer_token"));
        assert!(raw.contains("env:GOOGLE_OAUTH_ACCESS_TOKEN"));
        assert!(!raw.contains("ya29."));

        let loaded = AuthStore::load_from(&path).unwrap();
        assert_eq!(
            loaded.profiles["google-default"].method,
            AuthMethod::BearerToken
        );
        assert_eq!(
            loaded.profiles["google-default"].resolved_env_var(),
            Some("GOOGLE_OAUTH_ACCESS_TOKEN")
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn bearer_token_profiles_require_base_url() {
        let mut store = AuthStore::default();

        let result = store.upsert_bearer_token_env_profile(
            "google-default",
            "google-gemini",
            "GOOGLE_OAUTH_ACCESS_TOKEN",
            None,
        );

        assert!(result.is_err());
    }

    #[test]
    fn resolve_api_key_rejects_bearer_profiles() {
        let mut store = AuthStore::default();
        store
            .upsert_bearer_token_env_profile(
                "google-default",
                "google-gemini",
                "GOOGLE_OAUTH_ACCESS_TOKEN",
                Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            )
            .unwrap();

        assert!(store.resolve_api_key("google-default").is_err());
    }

    #[test]
    fn load_rejects_bearer_profile_without_base_url() {
        let path = temp_auth_path("broken_bearer_load");
        let _ = fs::remove_file(&path);
        fs::write(
            &path,
            r#"{
  "version": 1,
  "profiles": {
    "broken-bearer": {
      "provider": "google-gemini",
      "method": "bearer_token",
      "base_url": null,
      "secret_ref": "env:GOOGLE_OAUTH_ACCESS_TOKEN",
      "disabled": false
    }
  }
}"#,
        )
        .unwrap();

        let result = AuthStore::load_from(&path);

        assert!(result.is_err());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn resolve_auth_token_rejects_bearer_profile_without_base_url() {
        let profile = AuthProfile {
            provider: "google-gemini".to_string(),
            method: AuthMethod::BearerToken,
            base_url: None,
            secret_ref: "env:GOOGLE_OAUTH_ACCESS_TOKEN".to_string(),
            disabled: false,
        };

        assert!(profile.resolve_auth_token().is_err());
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
