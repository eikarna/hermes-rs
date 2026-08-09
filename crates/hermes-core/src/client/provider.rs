//! Provider abstraction layer for model-agnostic LLM access.
//!
//! `LLMProvider` is the trait all backends implement. `ProviderKind` selects a
//! backend from `[client] provider = "..."`, and `build_provider_client`
//! constructs the concrete client while applying per-provider endpoint and
//! capability defaults.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::client::{AnthropicClient, ChatResponse, ChatStreamResponse, Message, OpenAIClient};
use crate::error::Result;
use crate::schema::ToolSchema;

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

/// Look up per-model capabilities from [`CAPABILITY_TABLE`] by longest
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
}

impl ProviderKind {
    /// Parse from the string found in `[client] provider = "..."`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "openai" => Some(Self::Openai),
            "anthropic" => Some(Self::Anthropic),
            "ollama" => Some(Self::Ollama),
            "openrouter" => Some(Self::Openrouter),
            _ => None,
        }
    }

    /// Parse a configured provider name with a field-aware error.
    pub fn parse_configured(name: &str) -> Result<Self> {
        Self::from_name(name).ok_or_else(|| {
            crate::error::Error::Config(format!(
                "Unsupported client provider '{}'. Expected one of: openai, anthropic, ollama, openrouter.",
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
        }
    }
}

/// A model-agnostic chat interface implemented by every provider adapter.
#[async_trait]
pub trait LLMProvider: Send + Sync {
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
}

#[async_trait]
impl LLMProvider for ProviderClient {
    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        match self {
            Self::Openai(c) | Self::Ollama(c) | Self::Openrouter(c) => {
                c.chat(model, messages, tools).await
            }
            Self::Anthropic(c) => c.chat(model, messages, tools).await,
        }
    }

    async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        match self {
            Self::Openai(c) | Self::Ollama(c) | Self::Openrouter(c) => {
                c.chat_streaming(model, messages, tools).await
            }
            Self::Anthropic(c) => c.chat_streaming(model, messages, tools).await,
        }
    }

    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        match self {
            Self::Openai(c) | Self::Ollama(c) | Self::Openrouter(c) => c.capabilities(model),
            Self::Anthropic(c) => c.capabilities(model),
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
        || std::env::var("HERMES_AUTH_REF")
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
        ProviderKind::Openai | ProviderKind::Ollama | ProviderKind::Openrouter => {
            Arc::new(ProviderClient::from_openai_compatible(kind, client_config))
        }
        ProviderKind::Anthropic => Arc::new(AnthropicClient::new(client_config)?),
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
            _ => unreachable!("from_openai_compatible only accepts non-Anthropic providers"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
