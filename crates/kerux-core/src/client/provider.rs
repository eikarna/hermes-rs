//! Provider abstraction layer for model-agnostic LLM access.
//!
//! `LLMProvider` is the trait all backends implement. `ProviderKind` selects a
//! backend from `[client] provider = "..."`, and `build_provider_client`
//! constructs the concrete client while applying per-provider endpoint and
//! capability defaults.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::client::{AnthropicClient, ChatResponse, ChatStreamResponse, Message, OpenAIClient};
use crate::error::{Error, Result};
use crate::schema::ToolSchema;

/// Provider-reported model metadata used by discovery and onboarding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_window: Option<u64>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub pricing: Option<serde_json::Value>,
    pub raw: serde_json::Value,
}

/// Preferred code-edit format a provider/model supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    /// Aider-style SEARCH/REPLACE blocks (token-efficient).
    SearchReplace,
    /// Targeted find-and-replace via the existing `patch` tool.
    Patch,
    /// Full-file overwrite via the `file_write` tool.
    #[default]
    FullFile,
}

/// Runtime capabilities negotiated for a specific (provider, model) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderCapabilities {
    pub max_input_tokens: usize,
    pub max_output_tokens: usize,
    pub edit_format: EditFormat,
    pub supports_streaming: bool,
    pub supports_reasoning: bool,
    /// Whether the model accepts image inputs.
    pub supports_vision: bool,
    /// Whether the model emits native structured tool calls.
    pub supports_tool_calls: bool,
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            max_input_tokens: 128_000,
            max_output_tokens: 16_384,
            edit_format: EditFormat::FullFile,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: false,
            supports_tool_calls: true,
        }
    }
}

/// Per-model capability rows. A row applies to any model whose name starts
/// with the prefix (case-insensitive); the longest matching prefix wins.
/// Rows are provider-agnostic defaults — adapters merge these over their own
/// baseline in [`lookup_capabilities`].
const CAPABILITY_TABLE: &[(&str, ProviderCapabilities)] = &[
    // Google Gemini (recent first: longest-prefix lookup means 2.5 beats 2.0/1.5)
    (
        "gemini-2.5-pro",
        ProviderCapabilities {
            max_input_tokens: 1_048_576,
            max_output_tokens: 65_536,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "gemini-2.5-flash",
        ProviderCapabilities {
            max_input_tokens: 1_048_576,
            max_output_tokens: 65_536,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "gemini-",
        ProviderCapabilities {
            max_input_tokens: 1_048_576,
            max_output_tokens: 8_192,
            edit_format: EditFormat::Patch,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    // Anthropic Claude
    (
        "claude-opus-4",
        ProviderCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 32_000,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "claude-sonnet-4",
        ProviderCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 64_000,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "claude-haiku",
        ProviderCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 8_192,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    // OpenAI
    (
        "gpt-4o",
        ProviderCapabilities {
            max_input_tokens: 128_000,
            max_output_tokens: 16_384,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "gpt-4.1",
        ProviderCapabilities {
            max_input_tokens: 1_000_000,
            max_output_tokens: 32_768,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "o1",
        ProviderCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 100_000,
            edit_format: EditFormat::Patch,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: false,
            supports_tool_calls: true,
        },
    ),
    (
        "o3",
        ProviderCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 100_000,
            edit_format: EditFormat::Patch,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "o4-mini",
        ProviderCapabilities {
            max_input_tokens: 200_000,
            max_output_tokens: 100_000,
            edit_format: EditFormat::Patch,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: true,
            supports_tool_calls: true,
        },
    ),
    (
        "gpt-3.5",
        ProviderCapabilities {
            max_input_tokens: 16_385,
            max_output_tokens: 4_096,
            edit_format: EditFormat::FullFile,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: false,
            supports_tool_calls: true,
        },
    ),
];

/// Look up per-model capabilities from the built-in capability table by longest
/// case-insensitive prefix match. Returns `None` for unknown models so the
/// caller can fall back to adapter defaults.
pub fn lookup_capabilities(model: &str) -> Option<ProviderCapabilities> {
    let model = model.to_ascii_lowercase();
    CAPABILITY_TABLE
        .iter()
        .filter(|(prefix, _)| model.starts_with(&prefix.to_ascii_lowercase()))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, caps)| caps.clone())
}

/// Stable identifier for a provider backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,
    Anthropic,
    Ollama,
    Openrouter,
    Gemini,
    /// Nous Portal: OAuth-authenticated, OpenAI-compatible inference API.
    Nous,
}

impl ProviderKind {
    /// Parse from the string found in `[client] provider = "..."`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "openai" => Some(Self::Openai),
            "anthropic" => Some(Self::Anthropic),
            "ollama" => Some(Self::Ollama),
            "openrouter" => Some(Self::Openrouter),
            "gemini" | "google" => Some(Self::Gemini),
            "nous" => Some(Self::Nous),
            _ => None,
        }
    }

    /// Parse a configured provider name with a field-aware error.
    pub fn parse_configured(name: &str) -> Result<Self> {
        Self::from_name(name).ok_or_else(|| {
            crate::error::Error::Config(format!(
                "Unsupported client provider '{}'. Expected one of: openai, anthropic, ollama, openrouter, gemini, nous.",
                name
            ))
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Openrouter => "openrouter",
            Self::Gemini => "gemini",
            Self::Nous => "nous",
        }
    }
}

/// A model-agnostic chat interface implemented by every provider adapter.
#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Err(crate::error::Error::Agent(
            "model listing is not implemented for this provider".into(),
        ))
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse>;

    async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse>;

    fn capabilities(&self, model: &str) -> ProviderCapabilities;
}

/// Connection settings a concrete provider adapter needs.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
            timeout: Duration::from_secs(60),
        }
    }
}

/// Fully-resolved provider connection settings.
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub config: ProviderConfig,
    pub max_context_length: usize,
}

/// Boxed LLM backend selected by config.
pub enum ProviderClient {
    Openai(OpenAIClient),
    Ollama(OpenAIClient),
    Openrouter(OpenAIClient),
    Anthropic(AnthropicClient),
    Gemini(crate::client::gemini::GeminiClient),
    Nous(OpenAIClient),
}

#[async_trait]
impl LLMProvider for ProviderClient {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        match self {
            Self::Openai(c) | Self::Openrouter(c) | Self::Nous(c) => c.list_models().await,
            Self::Ollama(c) => c.list_ollama_models().await,
            Self::Anthropic(c) => c.list_models().await,
            Self::Gemini(c) => c.list_models().await,
        }
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        match self {
            Self::Openai(c) | Self::Ollama(c) | Self::Openrouter(c) | Self::Nous(c) => {
                c.chat(model, messages, tools).await
            }
            Self::Anthropic(c) => c.chat(model, messages, tools).await,
            Self::Gemini(c) => c.chat(model, messages, tools).await,
        }
    }

    async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        match self {
            Self::Openai(c) | Self::Ollama(c) | Self::Openrouter(c) | Self::Nous(c) => {
                c.chat_streaming(model, messages, tools).await
            }
            Self::Anthropic(c) => c.chat_streaming(model, messages, tools).await,
            Self::Gemini(c) => c.chat_streaming(model, messages, tools).await,
        }
    }

    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        match self {
            Self::Openai(c) | Self::Ollama(c) | Self::Openrouter(c) | Self::Nous(c) => {
                c.capabilities(model)
            }
            Self::Anthropic(c) => c.capabilities(model),
            Self::Gemini(c) => c.capabilities(model),
        }
    }
}

/// Resolve provider connection settings from `[client]`, honoring the
/// per-provider override subtables and environment variables.
pub fn resolve_provider_settings(
    settings: &crate::config::ClientSettings,
) -> Result<ProviderSettings> {
    let kind = ProviderKind::parse_configured(&settings.provider)?;

    let default_base_url = match kind {
        ProviderKind::Openai => "https://api.openai.com/v1",
        ProviderKind::Ollama => "http://localhost:11434/v1",
        ProviderKind::Openrouter => "https://openrouter.ai/api/v1",
        ProviderKind::Anthropic => "https://api.anthropic.com/v1",
        ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        ProviderKind::Nous => crate::auth::NOUS_INFERENCE_URL,
    };

    let base_url = settings
        .resolved_base_url_for(kind)
        .unwrap_or_else(|| default_base_url.to_string());
    let api_key = settings.resolved_api_key_for(kind).or_else(|| match kind {
        ProviderKind::Ollama => Some("ollama".to_string()), // required but ignored by Ollama
        _ => None,
    });
    let timeout_secs = settings
        .resolved_timeout_secs_for(kind)
        .unwrap_or(settings.timeout_secs);

    Ok(ProviderSettings {
        kind,
        config: ProviderConfig {
            base_url,
            api_key,
            timeout: Duration::from_secs(timeout_secs),
        },
        max_context_length: settings.max_context_length,
    })
}

/// Build the configured provider client using fully-resolved client settings.
pub fn build_provider_client(
    settings: &crate::config::ClientSettings,
) -> Result<Arc<dyn LLMProvider>> {
    // Preserve legacy behavior: when any auth profile is referenced (via
    // config or env), the shared OpenAI-compatible client is used so the
    // profile credential is never sent to a mismatched provider endpoint.
    let has_auth_ref = settings.auth_ref.is_some()
        || std::env::var("KERUX_AUTH_REF")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some();
    if has_auth_ref {
        return Ok(Arc::new(OpenAIClient::from_env()?));
    }

    let resolved = resolve_provider_settings(settings)?;
    let client_config = crate::client::ClientConfig {
        base_url: resolved.config.base_url,
        api_key: resolved.config.api_key,
        timeout: resolved.config.timeout,
        max_context_length: resolved.max_context_length,
    };
    build_provider_for_kind(resolved.kind, client_config)
}

/// Build a provider client after caller-specific settings (for example CLI auth
/// profiles) have already been applied.
pub fn build_provider_for_kind(
    kind: ProviderKind,
    client_config: crate::client::ClientConfig,
) -> Result<Arc<dyn LLMProvider>> {
    let client: Arc<dyn LLMProvider> = match kind {
        ProviderKind::Openai
        | ProviderKind::Ollama
        | ProviderKind::Openrouter
        | ProviderKind::Nous => {
            Arc::new(ProviderClient::from_openai_compatible(kind, client_config))
        }
        ProviderKind::Anthropic => Arc::new(AnthropicClient::new(client_config)?),
        ProviderKind::Gemini => Arc::new(crate::client::gemini::GeminiClient::new(client_config)?),
    };
    Ok(client)
}

impl ProviderClient {
    fn from_openai_compatible(
        kind: ProviderKind,
        config: crate::client::ClientConfig,
    ) -> ProviderClient {
        match kind {
            ProviderKind::Openai => ProviderClient::Openai(OpenAIClient::new(config)),
            ProviderKind::Ollama => ProviderClient::Ollama(OpenAIClient::new(config)),
            ProviderKind::Openrouter => ProviderClient::Openrouter(OpenAIClient::new(config)),
            ProviderKind::Nous => ProviderClient::Nous(OpenAIClient::new(config)),
            _ => unreachable!("from_openai_compatible only accepts non-Anthropic providers"),
        }
    }
}

/// File-backed model-list cache keyed by (provider, endpoint).
///
/// One JSON document per provider/endpoint hash under `model-cache/`.
/// Entries carry a Unix timestamp; reads older than `ttl` are treated
/// as misses. Corrupt or unreadable files degrade to a cache miss.
#[derive(Debug, Clone)]
pub struct ModelCache {
    dir: PathBuf,
    ttl: Duration,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedModelList {
    fetched_at_unix: u64,
    models: Vec<ModelInfo>,
}

impl ModelCache {
    pub fn new(dir: PathBuf, ttl: Duration) -> Self {
        Self { dir, ttl }
    }

    /// Default cache location: `<KERUX_HOME>/model-cache`.
    pub fn default_location() -> Self {
        Self::new(
            crate::platform::kerux_home().join("model-cache"),
            Duration::from_secs(3600),
        )
    }

    fn path(&self, kind: ProviderKind, endpoint: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"|");
        hasher.update(endpoint.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        self.dir.join(format!("{}.json", &hash[..16]))
    }

    /// Read a fresh entry; returns `None` on miss, expiry, or corruption.
    pub fn load(&self, kind: ProviderKind, endpoint: &str) -> Option<Vec<ModelInfo>> {
        let path = self.path(kind, endpoint);
        let raw = std::fs::read_to_string(&path).ok()?;
        let cached: CachedModelList = serde_json::from_str(&raw).ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        if now.saturating_sub(cached.fetched_at_unix) >= self.ttl.as_secs() {
            return None;
        }
        Some(cached.models)
    }

    /// Persist a model list with the current timestamp.
    pub fn store(&self, kind: ProviderKind, endpoint: &str, models: &[ModelInfo]) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::Agent(format!("system clock before Unix epoch: {e}")))?
            .as_secs();
        self.store_at(kind, endpoint, models, now)
    }

    /// Persist with an explicit timestamp (test seam for TTL checks).
    pub fn store_at(
        &self,
        kind: ProviderKind,
        endpoint: &str,
        models: &[ModelInfo],
        fetched_at_unix: u64,
    ) -> Result<()> {
        let path = self.path(kind, endpoint);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Agent(format!("model cache dir: {e}")))?;
        }
        let payload = CachedModelList {
            fetched_at_unix,
            models: models.to_vec(),
        };
        let json = serde_json::to_string_pretty(&payload)
            .map_err(|e| Error::Agent(format!("model cache serialize: {e}")))?;
        std::fs::write(&path, json).map_err(|e| Error::Agent(format!("model cache write: {e}")))?;
        Ok(())
    }
}

/// Fetch a provider's model list with cache-first semantics.
///
/// Fresh cache entries short-circuit the network unless `force_refresh`
/// is set. The fetch is bounded by `timeout`; on timeout the error names
/// the provider so the wizard can fall back to manual entry.
pub async fn discover_models(
    provider: &dyn LLMProvider,
    cache: &ModelCache,
    kind: ProviderKind,
    endpoint: &str,
    force_refresh: bool,
    timeout: Duration,
) -> Result<Vec<ModelInfo>> {
    if !force_refresh {
        if let Some(cached) = cache.load(kind, endpoint) {
            return Ok(cached);
        }
    }

    let fetch = provider.list_models();
    let models = match tokio::time::timeout(timeout, fetch).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(Error::Agent(format!(
                "model list for {} timed out after {}s",
                kind.as_str(),
                timeout.as_secs()
            )));
        }
    };

    // Cache write failures must not break discovery.
    let _ = cache.store(kind, endpoint, &models);
    Ok(models)
}

/// Wizard-friendly discovery: never fails. On any error returns an empty
/// list plus the error message so the caller can fall back to manual entry.
pub async fn discover_models_or_empty(
    provider: &dyn LLMProvider,
    cache: &ModelCache,
    kind: ProviderKind,
    endpoint: &str,
    force_refresh: bool,
    timeout: Duration,
) -> (Vec<ModelInfo>, Option<String>) {
    match discover_models(provider, cache, kind, endpoint, force_refresh, timeout).await {
        Ok(models) => (models, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientConfig;
    use crate::config::ClientSettings;

    #[test]
    fn provider_kind_from_name_roundtrip() {
        for kind in &[
            ProviderKind::Openai,
            ProviderKind::Anthropic,
            ProviderKind::Ollama,
            ProviderKind::Openrouter,
        ] {
            assert_eq!(ProviderKind::from_name(kind.as_str()), Some(*kind));
        }
    }

    #[test]
    fn provider_kind_from_name_case_insensitive() {
        assert_eq!(
            ProviderKind::from_name("OPENAI"),
            Some(ProviderKind::Openai)
        );
        assert_eq!(
            ProviderKind::from_name("AnThRoPiC"),
            Some(ProviderKind::Anthropic)
        );
    }

    #[test]
    fn provider_kind_from_name_invalid() {
        assert_eq!(ProviderKind::from_name("invalid"), None);
    }

    #[test]
    fn provider_kind_parse_configured_returns_field_aware_error() {
        let error = ProviderKind::parse_configured("unknown").unwrap_err();
        assert!(error
            .to_string()
            .contains("Unsupported client provider 'unknown'"));
    }

    #[test]
    fn edit_format_default() {
        assert_eq!(EditFormat::default(), EditFormat::FullFile);
    }

    #[test]
    fn provider_capabilities_default() {
        let caps = ProviderCapabilities::default();
        assert_eq!(caps.max_input_tokens, 128_000);
        assert_eq!(caps.max_output_tokens, 16_384);
        assert_eq!(caps.edit_format, EditFormat::FullFile);
        assert!(caps.supports_streaming);
        assert!(!caps.supports_reasoning);
    }

    #[test]
    fn provider_capabilities_serde() {
        let caps = ProviderCapabilities {
            max_input_tokens: 8_000,
            max_output_tokens: 512,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: false,
            supports_reasoning: true,
            supports_vision: false,
            supports_tool_calls: true,
        };
        let json = serde_json::to_string(&caps).unwrap();
        let parsed: ProviderCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.edit_format, EditFormat::SearchReplace);
        assert!(parsed.supports_reasoning);
    }

    #[test]
    fn resolve_provider_settings_uses_openai_defaults() {
        let settings = ClientSettings::default();
        let resolved = resolve_provider_settings(&settings).unwrap();
        assert_eq!(resolved.kind, ProviderKind::Openai);
        assert_eq!(resolved.config.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn lookup_capabilities_matches_longest_prefix() {
        let caps = lookup_capabilities("claude-sonnet-4-20250514").unwrap();
        assert_eq!(caps.max_input_tokens, 200_000);
        assert_eq!(caps.max_output_tokens, 64_000);
        assert_eq!(caps.edit_format, EditFormat::SearchReplace);
        assert!(caps.supports_reasoning);

        // "o4-mini" is longer than "o4"/"o1"; verify exact row wins
        let caps = lookup_capabilities("o4-mini-2025-04-16").unwrap();
        assert_eq!(caps.edit_format, EditFormat::Patch);
        assert!(caps.supports_reasoning);
    }

    #[test]
    fn lookup_capabilities_is_case_insensitive_and_unknown_returns_none() {
        assert!(lookup_capabilities("GPT-4O-MINI").is_some());
        assert!(lookup_capabilities("totally-unknown-model").is_none());
    }

    #[test]
    fn resolve_provider_settings_uses_ollama_defaults() {
        let settings = ClientSettings {
            provider: "ollama".to_string(),
            ..ClientSettings::default()
        };
        let resolved = resolve_provider_settings(&settings).unwrap();
        assert_eq!(resolved.kind, ProviderKind::Ollama);
        assert_eq!(resolved.config.base_url, "http://localhost:11434/v1");
        assert_eq!(resolved.config.api_key.as_deref(), Some("ollama"));
    }

    #[tokio::test]
    async fn openai_model_list_parses_openrouter_metadata_and_minimal_rows() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .match_header("authorization", "Bearer test-key")
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "data": [
                        {
                            "id": "vendor/rich-model",
                            "name": "Rich Model",
                            "context_length": 131072,
                            "architecture": {
                                "input_modalities": ["text", "image"],
                                "output_modalities": ["text"]
                            },
                            "pricing": {"prompt": "0.000001"}
                        },
                        {"id": "minimal-model"}
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let client = OpenAIClient::new(ClientConfig {
            base_url: format!("{}/v1", server.url()),
            api_key: Some("test-key".to_string()),
            timeout: Duration::from_secs(5),
            max_context_length: 128_000,
        });

        let models = client.list_models().await.unwrap();

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "vendor/rich-model");
        assert_eq!(models[0].display_name, "Rich Model");
        assert_eq!(models[0].context_window, Some(131_072));
        assert_eq!(models[0].input_modalities, ["text", "image"]);
        assert_eq!(models[0].output_modalities, ["text"]);
        assert!(models[0].pricing.is_some());
        assert_eq!(models[1].display_name, "minimal-model");
        assert!(models[1].input_modalities.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn anthropic_model_list_routes_through_provider_trait() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .match_header("x-api-key", "anthropic-key")
            .match_header("anthropic-version", "2023-06-01")
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "data": [{
                        "id": "claude-sonnet-4-20250514",
                        "display_name": "Claude Sonnet 4",
                        "type": "model"
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let client = AnthropicClient::new(ClientConfig {
            base_url: format!("{}/v1", server.url()),
            api_key: Some("anthropic-key".to_string()),
            timeout: Duration::from_secs(5),
            max_context_length: 200_000,
        })
        .unwrap();
        let provider: &dyn LLMProvider = &client;

        let models = provider.list_models().await.unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-4-20250514");
        assert_eq!(models[0].display_name, "Claude Sonnet 4");
        assert_eq!(models[0].context_window, None);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn gemini_model_list_normalizes_resource_names() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1beta/models")
            .match_query(mockito::Matcher::UrlEncoded(
                "key".into(),
                "gemini-key".into(),
            ))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "models": [{
                        "name": "models/gemini-2.5-pro",
                        "displayName": "Gemini 2.5 Pro",
                        "inputTokenLimit": 1048576,
                        "inputModalities": ["text", "image"],
                        "outputModalities": ["text"],
                        "supportedGenerationMethods": ["generateContent"]
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let client = crate::client::GeminiClient::new(ClientConfig {
            base_url: format!("{}/v1beta", server.url()),
            api_key: Some("gemini-key".to_string()),
            timeout: Duration::from_secs(5),
            max_context_length: 1_048_576,
        })
        .unwrap();

        let models = client.list_models().await.unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-2.5-pro");
        assert_eq!(models[0].display_name, "Gemini 2.5 Pro");
        assert_eq!(models[0].context_window, Some(1_048_576));
        assert_eq!(models[0].input_modalities, ["text", "image"]);
        assert_eq!(models[0].output_modalities, ["text"]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn ollama_model_list_uses_native_tags_endpoint() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/tags")
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "models": [{
                        "name": "qwen3:8b",
                        "model": "qwen3:8b",
                        "size": 5234567890_u64,
                        "digest": "sha256:abc",
                        "details": {"family": "qwen3", "parameter_size": "8.2B"}
                    }]
                })
                .to_string(),
            )
            .create_async()
            .await;
        let client = ProviderClient::from_openai_compatible(
            ProviderKind::Ollama,
            ClientConfig {
                base_url: format!("{}/v1", server.url()),
                api_key: Some("ollama".to_string()),
                timeout: Duration::from_secs(5),
                max_context_length: 128_000,
            },
        );

        let models = client.list_models().await.unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "qwen3:8b");
        assert_eq!(models[0].display_name, "qwen3:8b");
        assert_eq!(models[0].raw["details"]["family"], "qwen3");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn model_cache_reuses_fresh_data_and_force_refreshes() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_body(serde_json::json!({"data": [{"id": "cached-model"}]}).to_string())
            .expect(2)
            .create_async()
            .await;
        let client = OpenAIClient::new(ClientConfig {
            base_url: format!("{}/v1", server.url()),
            api_key: None,
            timeout: Duration::from_secs(5),
            max_context_length: 128_000,
        });
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(cache_dir.path().to_path_buf(), Duration::from_secs(3600));

        let first = discover_models(
            &client,
            &cache,
            ProviderKind::Openai,
            &format!("{}/v1", server.url()),
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let cached = discover_models(
            &client,
            &cache,
            ProviderKind::Openai,
            &format!("{}/v1", server.url()),
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let refreshed = discover_models(
            &client,
            &cache,
            ProviderKind::Openai,
            &format!("{}/v1", server.url()),
            true,
            Duration::from_secs(5),
        )
        .await
        .unwrap();

        assert_eq!(first, cached);
        assert_eq!(cached, refreshed);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn expired_model_cache_fetches_again() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_body(serde_json::json!({"data": [{"id": "live-model"}]}).to_string())
            .create_async()
            .await;
        let client = OpenAIClient::new(ClientConfig {
            base_url: format!("{}/v1", server.url()),
            api_key: None,
            timeout: Duration::from_secs(5),
            max_context_length: 128_000,
        });
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(cache_dir.path().to_path_buf(), Duration::from_secs(3600));
        cache
            .store_at(
                ProviderKind::Openai,
                &format!("{}/v1", server.url()),
                &[ModelInfo {
                    id: "expired-model".into(),
                    display_name: "expired-model".into(),
                    context_window: None,
                    input_modalities: Vec::new(),
                    output_modalities: Vec::new(),
                    pricing: None,
                    raw: serde_json::json!({"id": "expired-model"}),
                }],
                3_601,
            )
            .unwrap();

        let models = discover_models(
            &client,
            &cache,
            ProviderKind::Openai,
            &format!("{}/v1", server.url()),
            false,
            Duration::from_secs(5),
        )
        .await
        .unwrap();

        assert_eq!(models[0].id, "live-model");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn model_discovery_times_out_slow_endpoints() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/v1/models")
            .with_chunked_body(|w| {
                w.write_all(b"{\"data\":[")?;
                std::thread::sleep(Duration::from_secs(5));
                w.write_all(b"]}")
            })
            .create_async()
            .await;
        let client = OpenAIClient::new(ClientConfig {
            base_url: format!("{}/v1", server.url()),
            api_key: None,
            timeout: Duration::from_secs(30),
            max_context_length: 128_000,
        });
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(cache_dir.path().to_path_buf(), Duration::from_secs(3600));

        let result = discover_models(
            &client,
            &cache,
            ProviderKind::Openai,
            &format!("{}/v1", server.url()),
            false,
            Duration::from_millis(250),
        )
        .await;

        match result {
            Err(crate::error::Error::Agent(message)) => {
                assert!(
                    message.contains("timed out"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected timeout error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failed_discovery_returns_empty_list_with_error_context() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/v1/models")
            .with_status(500)
            .with_body("upstream exploded")
            .create_async()
            .await;
        let client = OpenAIClient::new(ClientConfig {
            base_url: format!("{}/v1", server.url()),
            api_key: None,
            timeout: Duration::from_secs(5),
            max_context_length: 128_000,
        });
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(cache_dir.path().to_path_buf(), Duration::from_secs(3600));

        let (models, error_context) = discover_models_or_empty(
            &client,
            &cache,
            ProviderKind::Openai,
            &format!("{}/v1", server.url()),
            false,
            Duration::from_secs(5),
        )
        .await;

        assert!(models.is_empty());
        let context = error_context.expect("failed fetch must carry error context");
        assert!(context.contains("500"), "unexpected context: {context}");
    }
}
