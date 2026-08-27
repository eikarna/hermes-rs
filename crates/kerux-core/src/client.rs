//! OpenAI-compatible client with SSE streaming support
//!
//! A lightweight, custom implementation using reqwest and serde.
//! Supports Server-Sent Events for streaming responses.
//! Supports reasoning_content for extended-thinking models.

pub mod fallback;
pub mod gemini;
pub mod provider;
pub use fallback::{FallbackChainProvider, FallbackEntry};
pub use gemini::GeminiClient;
pub use provider::{
    build_provider_client, build_provider_for_kind, discover_models, discover_models_or_empty,
    resolve_provider_settings, EditFormat, LLMProvider, ModelCache, ModelInfo,
    ProviderCapabilities, ProviderClient, ProviderConfig, ProviderKind, ProviderSettings,
};

use async_trait::async_trait;
use std::collections::HashMap;
use std::env;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE},
    Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, error, info, instrument};

use crate::auth::{AuthMethod, AuthStore};
use crate::config::{runtime_config, ClientSettings};
use crate::error::{Error, Result};
use crate::schema::ToolSchema;

/// OpenAI API client configuration
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Base URL for the OpenAI-compatible API
    pub base_url: String,
    /// API key for authentication
    pub api_key: Option<String>,
    /// Default request timeout
    pub timeout: Duration,
    /// Maximum context length (for truncation warnings)
    pub max_context_length: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self::from(&runtime_config().client)
    }
}

impl From<&ClientSettings> for ClientConfig {
    fn from(settings: &ClientSettings) -> Self {
        Self {
            base_url: settings.base_url.clone(),
            api_key: settings.api_key.clone(),
            timeout: Duration::from_secs(settings.timeout_secs),
            max_context_length: settings.max_context_length,
        }
    }
}

/// OpenAI-compatible client for chat completions
#[derive(Debug, Clone)]
pub struct OpenAIClient {
    config: ClientConfig,
    http_client: Client,
}

impl OpenAIClient {
    /// Create a new OpenAI client
    pub fn new(config: ClientConfig) -> Self {
        // connect_timeout: give up fast if the host is unreachable.
        // read_timeout: per-read deadline, so long streaming responses
        // (reasoning models) never get cut off mid-stream — the deadline
        // only fires if the server goes silent.
        let http_client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(config.timeout)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            http_client,
        }
    }

    #[cfg(test)]
    pub(crate) fn config_clone(&self) -> ClientConfig {
        self.config.clone()
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self> {
        let base = runtime_config();
        let api_key = env_var_non_empty("OPENAI_API_KEY");

        let base_url = env_var_non_empty("OPENAI_BASE_URL").unwrap_or(base.client.base_url);

        let mut config = ClientConfig {
            base_url,
            api_key: api_key.or(base.client.api_key),
            timeout: Duration::from_secs(base.client.timeout_secs),
            max_context_length: base.client.max_context_length,
        };

        let auth_ref = env_var_non_empty("KERUX_AUTH_REF").or(base.client.auth_ref);

        if let Some(auth_ref) = auth_ref.as_deref() {
            let store = AuthStore::load_default()?;
            let profile = store
                .profiles
                .get(auth_ref)
                .ok_or_else(|| Error::MissingConfig {
                    key: format!("auth profile '{}'", auth_ref),
                })?;
            let trusted_base_url = profile
                .base_url
                .clone()
                .or_else(|| match profile.method {
                    AuthMethod::ApiKey if is_openai_provider(&profile.provider) => {
                        Some(ClientSettings::default().base_url)
                    }
                    AuthMethod::ApiKey => None,
                    AuthMethod::BearerToken => None,
                    AuthMethod::Oauth => {
                        profile.oauth.as_ref().map(|t| t.inference_base_url.clone())
                    }
                })
                .ok_or_else(|| {
                    Error::Config(format!("Auth profile '{}' requires a base URL", auth_ref))
                })?;
            let default_base_url = ClientSettings::default().base_url;
            if config.base_url != default_base_url && config.base_url != trusted_base_url {
                return Err(Error::Config(format!(
                    "Auth profile '{}' is bound to '{}'; refusing to send credentials to '{}'",
                    auth_ref, trusted_base_url, config.base_url
                )));
            }
            match profile.method {
                AuthMethod::ApiKey => {
                    config.api_key = Some(store.resolve_api_key(auth_ref)?);
                }
                AuthMethod::BearerToken => {
                    config.api_key = Some(store.resolve_auth_token(auth_ref)?);
                }
                AuthMethod::Oauth => {
                    // The live access token is minted/refreshed by the caller
                    // before constructing the client; nothing to resolve here.
                }
            }
            config.base_url = trusted_base_url;
        }

        Ok(Self::new(config))
    }

    /// Build authorization headers
    fn build_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();

        // Content type
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // Authorization
        if let Some(ref api_key) = self.config.api_key {
            let auth_value = format!("Bearer {}", api_key);
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&auth_value)
                    .map_err(|_| Error::Config("Invalid API key format".to_string()))?,
            );
        }

        Ok(headers)
    }

    /// Build the chat completions URL
    fn build_url(&self, endpoint: &str) -> Result<reqwest::Url> {
        let base = self.config.base_url.trim_end_matches('/');
        let url = format!("{}/chat/completions{}", base, endpoint);
        reqwest::Url::parse(&url).map_err(|e| Error::InvalidUrl(e.to_string()))
    }

    /// List models exposed by an OpenAI-compatible `/models` endpoint.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let base = self.config.base_url.trim_end_matches('/');
        let url = reqwest::Url::parse(&format!("{base}/models"))
            .map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let response = self
            .http_client
            .get(url)
            .headers(self.build_headers()?)
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }

        let payload: Value = serde_json::from_slice(&body)
            .map_err(|e| Error::ParseResponse(format!("model list: {e}")))?;
        let rows = payload
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::ParseResponse("model list is missing data array".into()))?;

        rows.iter()
            .map(|raw| {
                let id = raw
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::ParseResponse("model row is missing id".into()))?;
                let strings = |value: Option<&Value>| {
                    value
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                Ok(ModelInfo {
                    id: id.to_owned(),
                    display_name: raw
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                        .to_owned(),
                    context_window: raw.get("context_length").and_then(Value::as_u64),
                    input_modalities: strings(raw.pointer("/architecture/input_modalities")),
                    output_modalities: strings(raw.pointer("/architecture/output_modalities")),
                    pricing: raw.get("pricing").cloned(),
                    raw: raw.clone(),
                })
            })
            .collect()
    }

    /// List locally installed models from Ollama's native `/api/tags` endpoint.
    pub async fn list_ollama_models(&self) -> Result<Vec<ModelInfo>> {
        let base = self.config.base_url.trim_end_matches('/');
        let native_base = base.strip_suffix("/v1").unwrap_or(base);
        let url = reqwest::Url::parse(&format!("{native_base}/api/tags"))
            .map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let response = self.http_client.get(url).send().await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }

        let payload: Value = serde_json::from_slice(&body)
            .map_err(|e| Error::ParseResponse(format!("Ollama model list: {e}")))?;
        let rows = payload
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::ParseResponse("Ollama model list is missing models".into()))?;
        rows.iter()
            .map(|raw| {
                let id = raw
                    .get("model")
                    .or_else(|| raw.get("name"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::ParseResponse("Ollama model is missing name".into()))?;
                Ok(ModelInfo {
                    id: id.to_owned(),
                    display_name: raw
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                        .to_owned(),
                    context_window: raw.get("context_length").and_then(Value::as_u64),
                    input_modalities: Vec::new(),
                    output_modalities: Vec::new(),
                    pricing: None,
                    raw: raw.clone(),
                })
            })
            .collect()
    }

    /// Send a non-streaming chat completion request
    #[instrument(skip(self, messages, tools), fields(model = % model))]
    pub async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        let request = self.build_chat_request(model, messages, tools, false)?;

        let url = self.build_url("")?;
        let headers = self.build_headers()?;

        let response = self
            .http_client
            .post(url)
            .headers(headers)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            error!(status = %status, body = %body, "Chat request failed");
            return Err(Error::Http {
                status: status.as_u16(),
                body,
            });
        }

        let response: ChatResponse = serde_json::from_str(&body)
            .map_err(|e| Error::ParseResponse(format!("{}: {}", e, body)))?;

        debug!(usage = ?response.usage, "Chat response received");
        Ok(response)
    }

    /// Send a streaming chat completion request
    #[instrument(skip(self, messages, tools), fields(model = % model))]
    pub async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        let request = self.build_chat_request(model, messages, tools, true)?;

        let url = self.build_url("")?;
        let headers = self.build_headers()?;

        let response = self
            .http_client
            .post(url)
            .headers(headers)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            error!(status = %status, body = %body, "Streaming request failed");
            return Err(Error::Http {
                status: status.as_u16(),
                body,
            });
        }

        info!("Streaming connection established");
        let stream = response.bytes_stream();
        Ok(ChatStreamResponse::new(stream))
    }

    ///Build the chat request payload
    fn build_chat_request(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
        stream: bool,
    ) -> Result<serde_json::Value> {
        let mut request = json!({
            "model": model,
            "messages": messages.iter().map(|m| m.to_value()).collect::<Vec<_>>(),
            "stream": stream,
        });

        if stream {
            request["stream_options"] = json!({"include_usage": true});
        }

        if let Some(tools) = tools {
            if !tools.is_empty() {
                let tools_array: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters
                            }
                        })
                    })
                    .collect();
                request["tools"] = json!(tools_array);
            }
        }

        Ok(request)
    }
}

/// Anthropic Messages API adapter. Translates the OpenAI-shaped internal
/// types used by the agent loop into Anthropic's `/messages` format.
///
/// Phase 1 slice: native Messages endpoint (`{base_url}/messages`),
/// `x-api-key` + `anthropic-version: 2023-06-01` headers, OpenAI-equivalent
/// tool schema mapping, and response normalization. Cache-control and
/// extended-thinking settings are out of scope.
#[derive(Debug, Clone)]
pub struct AnthropicClient {
    config: ClientConfig,
    http_client: Client,
}

impl AnthropicClient {
    /// Create an Anthropic adapter from a generic client config. The
    /// `base_url` should end at the API prefix (e.g. `https://api.anthropic.com/v1`);
    /// `/messages` is appended when routing requests.
    pub fn new(config: ClientConfig) -> Result<Self> {
        let http_client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(config.timeout)
            .build()
            .map_err(|e| Error::Config(format!("Failed to create HTTP client: {}", e)))?;
        Ok(Self {
            config,
            http_client,
        })
    }

    fn build_headers(&self) -> Result<HeaderMap> {
        let api_key = self.config.api_key.as_deref().ok_or_else(|| {
            Error::Config(
                "Anthropic provider requires ANTHROPIC_API_KEY or [client.anthropic].api_key"
                    .to_string(),
            )
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            HeaderValue::from_str(api_key)
                .map_err(|_| Error::Config("Invalid Anthropic API key format".to_string()))?,
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        Ok(headers)
    }

    fn messages_url(&self) -> Result<reqwest::Url> {
        let base = self.config.base_url.trim_end_matches('/');
        let url = format!("{}/messages", base);
        reqwest::Url::parse(&url).map_err(|e| Error::InvalidUrl(e.to_string()))
    }

    /// List models exposed by Anthropic's `/models` endpoint.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let base = self.config.base_url.trim_end_matches('/');
        let url = reqwest::Url::parse(&format!("{base}/models"))
            .map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let response = self
            .http_client
            .get(url)
            .headers(self.build_headers()?)
            .send()
            .await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }

        let payload: Value = serde_json::from_slice(&body)
            .map_err(|e| Error::ParseResponse(format!("Anthropic model list: {e}")))?;
        let rows = payload
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::ParseResponse("Anthropic model list is missing data".into()))?;
        rows.iter()
            .map(|raw| {
                let id = raw
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::ParseResponse("Anthropic model is missing id".into()))?;
                Ok(ModelInfo {
                    id: id.to_owned(),
                    display_name: raw
                        .get("display_name")
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                        .to_owned(),
                    context_window: raw.get("context_length").and_then(Value::as_u64),
                    input_modalities: Vec::new(),
                    output_modalities: Vec::new(),
                    pricing: raw.get("pricing").cloned(),
                    raw: raw.clone(),
                })
            })
            .collect()
    }

    /// Convert the internal OpenAI-shaped message list into Anthropic's
    /// `messages` + `system` shape. System messages are extracted into the
    /// `system` parameter; tool results become `tool_result` content blocks.
    fn build_request(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
        stream: bool,
    ) -> Result<serde_json::Value> {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut anthropic_messages: Vec<Value> = Vec::new();

        for m in messages {
            match m.role {
                Role::System => system_parts.push(m.content.trim()),
                Role::User => {
                    anthropic_messages.push(json!({
                        "role": "user",
                        "content": [{ "type": "text", "text": m.content }],
                    }));
                }
                Role::Assistant => {
                    let mut blocks: Vec<Value> = Vec::new();
                    if let Some(text) = Self::non_empty(&m.content) {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                    if let Some(calls) = m.tool_calls.as_deref() {
                        for call in calls {
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.function.name,
                                "input": serde_json::from_str::<Value>(&call.function.arguments)
                                   .unwrap_or_else(|_| json!({})),
                            }));
                        }
                    }
                    anthropic_messages.push(json!({ "role": "assistant", "content": blocks }));
                }
                Role::Tool => {
                    anthropic_messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                            "content": [{ "type": "text", "text": m.content }],
                        }],
                    }));
                }
            }
        }

        let mut request = json!({
            "model": model,
            "max_tokens": 16_384,
            "stream": stream,
            "messages": anthropic_messages,
        });
        if !system_parts.is_empty() {
            request["system"] = json!(system_parts.join("\n\n"));
        }
        if let Some(tools) = tools.filter(|t| !t.is_empty()) {
            request["tools"] = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect::<Vec<_>>()
                .into();
        }
        Ok(request)
    }

    fn non_empty(s: &str) -> Option<&str> {
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// Run a non-streaming chat completion.
    pub async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        let request = self.build_request(model, messages, tools, false)?;
        let response = self
            .http_client
            .post(self.messages_url()?)
            .headers(self.build_headers()?)
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            // Typed variant so fallback/retry classifiers can branch on the
            // status instead of parsing formatted strings.
            return Err(Error::Http {
                status: status.as_u16(),
                body,
            });
        }
        Self::parse_anthropic_response(&body, model)
    }

    /// Run Anthropic's streaming `/messages` API and expose the same SSE
    /// pipeline used for OpenAI-compatible responses.
    pub async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        let request = self.build_request(model, messages, tools, true)?;
        let response = self
            .http_client
            .post(self.messages_url()?)
            .headers(self.build_headers()?)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            // Same typed variant as non-streaming: 429/5xx must be visible
            // to the fallback classifier without string sniffing.
            return Err(Error::Http {
                status: status.as_u16(),
                body,
            });
        }
        Ok(ChatStreamResponse::new(response.bytes_stream()))
    }

    /// Convert Anthropic's response JSON into the shared `ChatResponse` type
    /// used throughout the agent loop. Only text and tool_use content blocks
    /// are mapped; citations/thinking blocks are dropped for Phase 1.
    fn parse_anthropic_response(body: &str, model: &str) -> Result<ChatResponse> {
        let value: Value = serde_json::from_str(body)
            .map_err(|e| Error::ParseResponse(format!("Invalid Anthropic response: {}", e)))?;

        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let stop_reason = value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(Self::normalize_stop_reason);

        let usage = value
            .get("usage")
            .map(|u| {
                let prompt_tokens =
                    u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                let completion_tokens =
                    u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens: prompt_tokens.saturating_add(completion_tokens),
                    cached_prompt_tokens: u
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                }
            })
            .unwrap_or_default();

        let content_blocks = value
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for block in content_blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    if let (Some(id), Some(name)) = (
                        block.get("id").and_then(Value::as_str),
                        block.get("name").and_then(Value::as_str),
                    ) {
                        let arguments = match block.get("input") {
                            Some(input) => serde_json::to_string(input).unwrap_or_default(),
                            None => "{}".to_string(),
                        };
                        tool_calls.push(ToolCall {
                            id: id.to_string(),
                            function: ToolCallFunction {
                                name: name.to_string(),
                                arguments,
                            },
                        });
                    }
                }
                _ => {}
            }
        }

        let message = MessageDelta {
            role: Some(Role::Assistant),
            content: if text.is_empty() { None } else { Some(text) },
            reasoning_content: None,
            tool_calls: None,
        };

        // Serialize tool calls separately through `with_tool_calls` so the
        // parsing logic stays identical to the OpenAI path.
        let mut message = message;
        if !tool_calls.is_empty() {
            let tool_calls_value: Vec<crate::client::ToolCallDelta> = tool_calls
                .into_iter()
                .map(|tc| crate::client::ToolCallDelta {
                    index: 0,
                    id: Some(tc.id),
                    call_type: Some("function".to_string()),
                    function: Some(tc.function),
                })
                .collect();
            message.tool_calls = Some(tool_calls_value);
        }

        Ok(ChatResponse {
            id,
            object: "chat.completion".to_string(),
            created: 0,
            model: model.to_string(),
            choices: vec![Choice {
                index: 0,
                message,
                finish_reason: stop_reason,
            }],
            usage,
        })
    }

    fn normalize_stop_reason(reason: &str) -> String {
        match reason {
            "end_turn" | "max_tokens" | "stop_sequence" | "tool_use" => "stop".to_string(),
            other => other.to_string(),
        }
    }
}

#[async_trait]
impl LLMProvider for AnthropicClient {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        AnthropicClient::list_models(self).await
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        self.chat(model, messages, tools).await
    }

    async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        self.chat_streaming(model, messages, tools).await
    }

    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        if let Some(table) = provider::lookup_capabilities(model) {
            return table;
        }
        ProviderCapabilities {
            max_input_tokens: self.config.max_context_length,
            max_output_tokens: 8_192,
            edit_format: EditFormat::SearchReplace,
            supports_streaming: true,
            supports_reasoning: true,
            supports_vision: false,
            supports_tool_calls: true,
        }
    }
}

/// Concrete per-model negotiation lands with the provider registry.
#[async_trait]
impl LLMProvider for OpenAIClient {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        OpenAIClient::list_models(self).await
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        self.chat(model, messages, tools).await
    }

    async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        self.chat_streaming(model, messages, tools).await
    }

    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        if let Some(table) = provider::lookup_capabilities(model) {
            return table;
        }
        ProviderCapabilities {
            max_input_tokens: self.config.max_context_length,
            max_output_tokens: 16_384,
            edit_format: EditFormat::FullFile,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: false,
            supports_tool_calls: true,
        }
    }
}

/// Run a chat completion through any [`LLMProvider`] with a single call.
/// Note: `chat` is the non-streaming path; prefer [`chat_streaming_with_provider`]
/// for streaming-first usage in the agent loop.
pub async fn chat_with_provider(
    provider: &dyn LLMProvider,
    model: &str,
    messages: &[Message],
    tools: Option<&[ToolSchema]>,
) -> Result<ChatResponse> {
    provider.chat(model, messages, tools).await
}

/// Run a streaming chat completion through any [`LLMProvider`].
pub async fn chat_streaming_with_provider(
    provider: &dyn LLMProvider,
    model: &str,
    messages: &[Message],
    tools: Option<&[ToolSchema]>,
) -> Result<ChatStreamResponse> {
    provider.chat_streaming(model, messages, tools).await
}

/// Chat message
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// A chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub reasoning: Option<String>,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    /// Create a new message
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            reasoning: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    /// Create a user message
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    /// Create an assistant message
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    /// Create a tool message
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            reasoning: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
            tool_calls: None,
        }
    }

    /// Add tool calls to the message
    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = Some(tool_calls);
        self
    }

    /// Add reasoning content to the message
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        let reasoning = reasoning.into();
        if !reasoning.trim().is_empty() {
            self.reasoning = Some(reasoning);
        }
        self
    }

    /// Convert to JSON value for API
    fn to_value(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("role".to_string(), json!(self.role.as_str()));

        if let Some(ref tool_calls) = self.tool_calls {
            let tc_array: Vec<Value> = tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.function.name,
                            "arguments": tc.function.arguments
                        }
                    })
                })
                .collect();
            map.insert("tool_calls".to_string(), json!(tc_array));
            map.insert("content".to_string(), json!(self.content));
        } else {
            map.insert("content".to_string(), json!(self.content));
        }

        if let Some(ref name) = self.name {
            map.insert("name".to_string(), json!(name));
        }
        if let Some(ref tool_call_id) = self.tool_call_id {
            map.insert("tool_call_id".to_string(), json!(tool_call_id));
        }

        Value::Object(map)
    }
}

impl Default for Message {
    fn default() -> Self {
        Self {
            role: Role::User,
            content: String::new(),
            reasoning: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }
}

/// A tool call from the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub function: ToolCallFunction,
}

/// Function in a tool call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// Chat completion response (non-streaming)
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

/// A completion choice
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub index: usize,
    pub message: MessageDelta,
    pub finish_reason: Option<String>,
}

/// Message delta from API (non-streaming)
#[derive(Debug, Clone, Deserialize)]
pub struct MessageDelta {
    pub role: Option<Role>,
    pub content: Option<String>,
    /// Reasoning content from extended-thinking models (e.g. DeepSeek, OpenAI o1)
    #[serde(
        default,
        alias = "reasoning_content",
        alias = "reasoning",
        alias = "reasoning_context"
    )]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// Tool call delta
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallDelta {
    #[serde(default)]
    pub index: usize,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    pub function: Option<ToolCallFunction>,
}

/// API usage statistics
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cached_prompt_tokens: u32,
}

#[derive(Deserialize)]
struct UsageWire {
    #[serde(default, alias = "input_tokens")]
    prompt_tokens: u32,
    #[serde(default, alias = "output_tokens")]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
    #[serde(default)]
    cached_prompt_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: PromptTokensDetails,
}

#[derive(Default, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}

impl<'de> Deserialize<'de> for Usage {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = UsageWire::deserialize(deserializer)?;
        let total_tokens = if wire.total_tokens == 0 {
            wire.prompt_tokens.saturating_add(wire.completion_tokens)
        } else {
            wire.total_tokens
        };
        Ok(Self {
            prompt_tokens: wire.prompt_tokens,
            completion_tokens: wire.completion_tokens,
            total_tokens,
            cached_prompt_tokens: wire
                .cached_prompt_tokens
                .max(wire.cache_read_input_tokens)
                .max(wire.prompt_tokens_details.cached_tokens),
        })
    }
}

/// SSE streaming event from the OpenAI API
#[derive(Debug, Clone, Deserialize)]
pub struct ChatStreamEvent {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A streaming choice
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    #[serde(default)]
    pub index: usize,
    pub delta: StreamingMessageDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Message delta from streaming API
#[derive(Debug, Clone, Deserialize)]
pub struct StreamingMessageDelta {
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub content: Option<String>,
    /// Reasoning content from extended-thinking models (e.g. DeepSeek, OpenAI o1)
    #[serde(
        default,
        alias = "reasoning_content",
        alias = "reasoning",
        alias = "reasoning_context"
    )]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamingToolCallDelta>>,
}

/// Tool call delta for streaming
#[derive(Debug, Clone, Deserialize)]
pub struct StreamingToolCallDelta {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type", default)]
    pub call_type: Option<String>,
    #[serde(default)]
    pub function: Option<ToolCallFunction>,
}

/// SSE streaming response wrapper
pub struct ChatStreamResponse {
    inner: Box<dyn Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin>,
    buffer: String,
    anthropic_tool_blocks: HashMap<usize, AnthropicToolBlock>,
}

#[derive(Debug, Clone)]
struct AnthropicToolBlock {
    id: String,
    name: String,
}

impl ChatStreamResponse {
    pub fn new(
        stream: impl Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
    ) -> Self {
        Self {
            inner: Box::new(stream),
            buffer: String::new(),
            anthropic_tool_blocks: HashMap::new(),
        }
    }
}

impl Stream for ChatStreamResponse {
    type Item = crate::error::Result<ChatStreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            if let Some(event) =
                try_parse_next_sse_event(&mut this.buffer, false, &mut this.anthropic_tool_blocks)
            {
                return Poll::Ready(Some(Ok(event)));
            }
            if has_complete_sse_event(&this.buffer) {
                continue;
            }

            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                        this.buffer.push_str(&text);
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(Error::Network(e)))),
                Poll::Ready(None) => {
                    return Poll::Ready(
                        try_parse_next_sse_event(
                            &mut this.buffer,
                            true,
                            &mut this.anthropic_tool_blocks,
                        )
                        .map(Ok),
                    );
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn env_var_non_empty(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_openai_provider(provider: &str) -> bool {
    provider.eq_ignore_ascii_case("OpenAI") || provider.eq_ignore_ascii_case("openai")
}

fn try_parse_next_sse_event(
    buffer: &mut String,
    allow_partial: bool,
    anthropic_tool_blocks: &mut HashMap<usize, AnthropicToolBlock>,
) -> Option<ChatStreamEvent> {
    normalize_sse_buffer(buffer);

    let event_end = if let Some(index) = buffer.find("\n\n") {
        index
    } else if allow_partial && !buffer.trim().is_empty() {
        buffer.len()
    } else {
        return None;
    };

    let event_data = buffer[..event_end].to_string();
    let drain_len = if event_end < buffer.len() {
        event_end + 2
    } else {
        event_end
    };
    buffer.drain(..drain_len);

    let mut event_name = None;
    let mut payload_lines = Vec::new();
    for line in event_data.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            payload_lines.push(value.trim_start());
        }
    }
    let payload = payload_lines.join("\n");

    if payload.is_empty() {
        return None;
    }

    if payload.trim() == "[DONE]" {
        return None;
    }

    match serde_json::from_str::<ChatStreamEvent>(payload.trim()) {
        Ok(event) => Some(event),
        Err(e) => {
            if let Some(json_start) = payload.find('{') {
                let potential_json = &payload[json_start..];
                if let Ok(event) = serde_json::from_str::<ChatStreamEvent>(potential_json.trim()) {
                    return Some(event);
                }
            }
            if let Some(event) = parse_anthropic_stream_event(
                event_name.as_deref(),
                payload.trim(),
                anthropic_tool_blocks,
            ) {
                return Some(event);
            }
            debug!(error = %e, payload = %payload, "Failed to parse SSE event");
            None
        }
    }
}

fn parse_anthropic_stream_event(
    event_name: Option<&str>,
    payload: &str,
    tool_blocks: &mut HashMap<usize, AnthropicToolBlock>,
) -> Option<ChatStreamEvent> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let event_type = value.get("type").and_then(Value::as_str).or(event_name)?;

    match event_type {
        "content_block_start" => parse_anthropic_content_block_start(&value, tool_blocks),
        "content_block_delta" => parse_anthropic_content_block_delta(&value, tool_blocks),
        "message_delta" => {
            let finish_reason = value
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            finish_reason
                .map(|finish_reason| normalized_stream_event(default_delta(), Some(finish_reason)))
        }
        "message_stop" => Some(normalized_stream_event(
            default_delta(),
            Some("stop".to_string()),
        )),
        _ => None,
    }
}

fn parse_anthropic_content_block_start(
    value: &Value,
    tool_blocks: &mut HashMap<usize, AnthropicToolBlock>,
) -> Option<ChatStreamEvent> {
    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let block = value.get("content_block")?;
    match block.get("type").and_then(Value::as_str)? {
        "text" => block
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| normalized_stream_event(delta_with_content(text), None)),
        "thinking" => block
            .get("thinking")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| normalized_stream_event(delta_with_reasoning(text), None)),
        "tool_use" => {
            let id = block.get("id")?.as_str()?.to_string();
            let name = block.get("name")?.as_str()?.to_string();
            tool_blocks.insert(
                index,
                AnthropicToolBlock {
                    id: id.clone(),
                    name: name.clone(),
                },
            );
            let arguments = block
                .get("input")
                .filter(|input| !input.is_null())
                .map(|input| input.to_string())
                .unwrap_or_else(|| "{}".to_string());
            Some(normalized_stream_event(
                delta_with_tool_call(index, id, name, arguments),
                None,
            ))
        }
        _ => None,
    }
}

fn parse_anthropic_content_block_delta(
    value: &Value,
    tool_blocks: &HashMap<usize, AnthropicToolBlock>,
) -> Option<ChatStreamEvent> {
    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let delta = value.get("delta")?;
    match delta.get("type").and_then(Value::as_str)? {
        "text_delta" => delta
            .get("text")
            .and_then(Value::as_str)
            .map(|text| normalized_stream_event(delta_with_content(text), None)),
        "thinking_delta" => delta
            .get("thinking")
            .and_then(Value::as_str)
            .map(|text| normalized_stream_event(delta_with_reasoning(text), None)),
        "input_json_delta" => {
            let tool = tool_blocks.get(&index)?;
            let arguments = delta
                .get("partial_json")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(normalized_stream_event(
                delta_with_tool_call(index, tool.id.clone(), tool.name.clone(), arguments),
                None,
            ))
        }
        _ => None,
    }
}

fn normalized_stream_event(
    delta: StreamingMessageDelta,
    finish_reason: Option<String>,
) -> ChatStreamEvent {
    ChatStreamEvent {
        id: String::new(),
        object: "chat.completion.chunk".to_string(),
        created: 0,
        model: String::new(),
        choices: vec![StreamChoice {
            index: 0,
            delta,
            finish_reason,
        }],
        usage: None,
    }
}

fn default_delta() -> StreamingMessageDelta {
    StreamingMessageDelta {
        role: None,
        content: None,
        reasoning_content: None,
        tool_calls: None,
    }
}

fn delta_with_content(content: &str) -> StreamingMessageDelta {
    StreamingMessageDelta {
        content: Some(content.to_string()),
        ..default_delta()
    }
}

fn delta_with_reasoning(reasoning: &str) -> StreamingMessageDelta {
    StreamingMessageDelta {
        reasoning_content: Some(reasoning.to_string()),
        ..default_delta()
    }
}

fn delta_with_tool_call(
    index: usize,
    id: String,
    name: String,
    arguments: String,
) -> StreamingMessageDelta {
    StreamingMessageDelta {
        tool_calls: Some(vec![StreamingToolCallDelta {
            index,
            id: Some(id),
            call_type: Some("function".to_string()),
            function: Some(ToolCallFunction { name, arguments }),
        }]),
        ..default_delta()
    }
}

fn normalize_sse_buffer(buffer: &mut String) {
    if buffer.contains('\r') {
        *buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
    }
}

fn has_complete_sse_event(buffer: &str) -> bool {
    buffer.contains("\n\n")
}

/// Builder for constructing messages
pub struct MessageBuilder {
    message: Message,
}

impl MessageBuilder {
    pub fn new(role: Role) -> Self {
        Self {
            message: Message::new(role, ""),
        }
    }

    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.message.content = content.into();
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.message.name = Some(name.into());
        self
    }

    pub fn tool_call_id(mut self, id: impl Into<String>) -> Self {
        self.message.tool_call_id = Some(id.into());
        self
    }

    pub fn tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.message.tool_calls = Some(tool_calls);
        self
    }

    pub fn build(self) -> Message {
        self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;
    use serial_test::serial;

    #[test]
    fn openai_usage_reports_cached_prompt_tokens() {
        let usage: Usage = serde_json::from_value(serde_json::json!({
            "prompt_tokens": 1_000,
            "completion_tokens": 250,
            "total_tokens": 1_250,
            "prompt_tokens_details": {"cached_tokens": 400}
        }))
        .unwrap();

        assert_eq!(usage.cached_prompt_tokens, 400);
    }

    #[test]
    fn streaming_request_asks_provider_for_usage() {
        let client = OpenAIClient::new(ClientConfig::default());
        let request = client
            .build_chat_request("gpt-4o", &[Message::user("hello")], None, true)
            .unwrap();

        assert_eq!(request["stream_options"]["include_usage"], true);
    }

    fn temp_auth_store_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("kerux_client_auth_{}_{}", name, std::process::id()))
            .join("auth.json")
    }

    fn cleanup_auth_store_path(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn test_message_to_value() {
        let msg = Message::user("Hello, world!");
        let value = msg.to_value();

        assert_eq!(value["role"], "user");
        assert_eq!(value["content"], "Hello, world!");
    }

    #[test]
    fn test_tool_message() {
        let msg = Message::tool("call_123", "Result: 42");
        let value = msg.to_value();

        assert_eq!(value["role"], "tool");
        assert_eq!(value["tool_call_id"], "call_123");
    }

    #[test]
    fn test_reasoning_context_alias_deserializes() {
        let value = serde_json::json!({
            "role": "assistant",
            "reasoning_context": "<think>checking</think>"
        });

        let delta: StreamingMessageDelta =
            serde_json::from_value(value).expect("streaming delta should deserialize");

        assert_eq!(
            delta.reasoning_content.as_deref(),
            Some("<think>checking</think>")
        );
    }

    #[test]
    fn streaming_parser_handles_crlf_events() {
        let mut buffer = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"demo\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\r\n\r\n".to_string();
        let event = try_parse_next_sse_event(&mut buffer, false, &mut HashMap::new())
            .expect("event should parse");

        assert_eq!(event.choices.len(), 1);
        assert_eq!(event.choices[0].delta.content.as_deref(), Some("Hello"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn streaming_parser_handles_partial_final_event() {
        let mut buffer = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"demo\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}]}".to_string();
        let event = try_parse_next_sse_event(&mut buffer, true, &mut HashMap::new())
            .expect("trailing event should parse");

        assert_eq!(event.choices[0].delta.content.as_deref(), Some("Done"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn streaming_parser_normalizes_anthropic_text_delta() {
        let mut buffer = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello Claude\"}}\n\n".to_string();
        let event = try_parse_next_sse_event(&mut buffer, false, &mut HashMap::new())
            .expect("anthropic text delta should parse");

        assert_eq!(
            event.choices[0].delta.content.as_deref(),
            Some("Hello Claude")
        );
    }

    #[test]
    fn streaming_parser_normalizes_anthropic_thinking_delta() {
        let mut buffer = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"checking\"}}\n\n".to_string();
        let event = try_parse_next_sse_event(&mut buffer, false, &mut HashMap::new())
            .expect("anthropic thinking delta should parse");

        assert_eq!(
            event.choices[0].delta.reasoning_content.as_deref(),
            Some("checking")
        );
    }

    #[test]
    fn streaming_parser_normalizes_anthropic_tool_use_deltas() {
        let mut tool_blocks = HashMap::new();
        let mut start = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n".to_string();
        let start_event = try_parse_next_sse_event(&mut start, false, &mut tool_blocks)
            .expect("anthropic tool start should parse");
        let start_call = start_event.choices[0].delta.tool_calls.as_ref().unwrap()[0].clone();
        assert_eq!(start_call.id.as_deref(), Some("toolu_1"));
        assert_eq!(start_call.function.as_ref().unwrap().name, "read_file");
        assert_eq!(start_call.function.as_ref().unwrap().arguments, "{}");

        let mut delta = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n".to_string();
        let delta_event = try_parse_next_sse_event(&mut delta, false, &mut tool_blocks)
            .expect("anthropic tool input delta should parse");
        let delta_call = delta_event.choices[0].delta.tool_calls.as_ref().unwrap()[0].clone();

        assert_eq!(delta_call.id.as_deref(), Some("toolu_1"));
        assert_eq!(delta_call.function.as_ref().unwrap().name, "read_file");
        assert_eq!(
            delta_call.function.as_ref().unwrap().arguments,
            "{\"path\":\"README.md\"}"
        );
    }

    #[tokio::test]
    async fn streaming_response_skips_ignored_anthropic_events_in_same_chunk() {
        use futures::StreamExt;

        let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> = vec![Ok(Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"after ignored\"}}\n\n",
        ))];
        let mut stream = ChatStreamResponse::new(futures::stream::iter(chunks));
        let event = stream
            .next()
            .await
            .expect("stream should yield normalized event")
            .expect("normalized event should parse");

        assert_eq!(
            event.choices[0].delta.content.as_deref(),
            Some("after ignored")
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_client_from_env() {
        // This will succeed even without env vars (uses defaults)
        let client = OpenAIClient::from_env();
        assert!(client.is_ok());
    }

    #[test]
    #[serial]
    fn client_from_env_resolves_auth_ref_profile() {
        let auth_store_path = temp_auth_store_path("auth_ref");
        cleanup_auth_store_path(&auth_store_path);
        let old_auth_store = std::env::var("KERUX_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("KERUX_AUTH_REF").ok();
        let old_api_key = std::env::var("KERUX_TEST_CLIENT_API_KEY").ok();
        let old_openai_api_key = std::env::var("OPENAI_API_KEY").ok();
        let old_base_url = std::env::var("OPENAI_BASE_URL").ok();

        std::env::set_var("KERUX_AUTH_STORE", &auth_store_path);
        std::env::set_var("KERUX_AUTH_REF", "test-default");
        std::env::set_var("KERUX_TEST_CLIENT_API_KEY", "profile-key");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("OPENAI_BASE_URL");

        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile(
                "test-default",
                "openai",
                "KERUX_TEST_CLIENT_API_KEY",
                Some("http://127.0.0.1:11434/v1".to_string()),
            )
            .unwrap();
        store.save_default().unwrap();

        let client = OpenAIClient::from_env().unwrap();
        let config = client.config_clone();
        assert_eq!(config.api_key.as_deref(), Some("profile-key"));
        assert_eq!(config.base_url, "http://127.0.0.1:11434/v1");

        restore_env("KERUX_AUTH_STORE", old_auth_store);
        restore_env("KERUX_AUTH_REF", old_auth_ref);
        restore_env("KERUX_TEST_CLIENT_API_KEY", old_api_key);
        restore_env("OPENAI_API_KEY", old_openai_api_key);
        restore_env("OPENAI_BASE_URL", old_base_url);
        cleanup_auth_store_path(&auth_store_path);
    }

    #[test]
    #[serial]
    fn client_from_env_rejects_untrusted_auth_profile_base_url_override() {
        let auth_store_path = temp_auth_store_path("auth_exfil");
        cleanup_auth_store_path(&auth_store_path);
        let old_auth_store = std::env::var("KERUX_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("KERUX_AUTH_REF").ok();
        let old_api_key = std::env::var("KERUX_TEST_CLIENT_API_KEY").ok();
        let old_openai_api_key = std::env::var("OPENAI_API_KEY").ok();
        let old_base_url = std::env::var("OPENAI_BASE_URL").ok();

        std::env::set_var("KERUX_AUTH_STORE", &auth_store_path);
        std::env::set_var("KERUX_AUTH_REF", "test-default");
        std::env::set_var("KERUX_TEST_CLIENT_API_KEY", "profile-key");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::set_var("OPENAI_BASE_URL", "https://attacker.example/v1");

        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile("test-default", "openai", "KERUX_TEST_CLIENT_API_KEY", None)
            .unwrap();
        store.save_default().unwrap();

        let result = OpenAIClient::from_env();
        assert!(result.is_err());

        restore_env("KERUX_AUTH_STORE", old_auth_store);
        restore_env("KERUX_AUTH_REF", old_auth_ref);
        restore_env("KERUX_TEST_CLIENT_API_KEY", old_api_key);
        restore_env("OPENAI_API_KEY", old_openai_api_key);
        restore_env("OPENAI_BASE_URL", old_base_url);
        cleanup_auth_store_path(&auth_store_path);
    }

    #[test]
    #[serial]
    fn client_from_env_uses_bearer_profile_token_over_openai_api_key() {
        let auth_store_path = temp_auth_store_path("bearer_auth");
        cleanup_auth_store_path(&auth_store_path);
        let old_auth_store = std::env::var("KERUX_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("KERUX_AUTH_REF").ok();
        let old_bearer = std::env::var("GOOGLE_OAUTH_ACCESS_TOKEN").ok();
        let old_openai_api_key = std::env::var("OPENAI_API_KEY").ok();
        let old_base_url = std::env::var("OPENAI_BASE_URL").ok();

        std::env::set_var("KERUX_AUTH_STORE", &auth_store_path);
        std::env::set_var("KERUX_AUTH_REF", "google-default");
        std::env::set_var("GOOGLE_OAUTH_ACCESS_TOKEN", "google-token");
        std::env::set_var("OPENAI_API_KEY", "openai-key");
        std::env::remove_var("OPENAI_BASE_URL");

        let mut store = AuthStore::default();
        store
            .upsert_bearer_token_env_profile(
                "google-default",
                "google-gemini",
                "GOOGLE_OAUTH_ACCESS_TOKEN",
                Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            )
            .unwrap();
        store.save_default().unwrap();

        let client = OpenAIClient::from_env().unwrap();
        let config = client.config_clone();
        assert_eq!(config.api_key.as_deref(), Some("google-token"));
        assert_eq!(
            config.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );

        restore_env("KERUX_AUTH_STORE", old_auth_store);
        restore_env("KERUX_AUTH_REF", old_auth_ref);
        restore_env("GOOGLE_OAUTH_ACCESS_TOKEN", old_bearer);
        restore_env("OPENAI_API_KEY", old_openai_api_key);
        restore_env("OPENAI_BASE_URL", old_base_url);
        cleanup_auth_store_path(&auth_store_path);
    }

    #[test]
    #[serial]
    fn client_from_env_uses_api_key_profile_over_openai_api_key() {
        let auth_store_path = temp_auth_store_path("google_api_key_precedence");
        cleanup_auth_store_path(&auth_store_path);
        let old_auth_store = std::env::var("KERUX_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("KERUX_AUTH_REF").ok();
        let old_google_api_key = std::env::var("GOOGLE_API_KEY").ok();
        let old_openai_api_key = std::env::var("OPENAI_API_KEY").ok();
        let old_base_url = std::env::var("OPENAI_BASE_URL").ok();

        std::env::set_var("KERUX_AUTH_STORE", &auth_store_path);
        std::env::set_var("KERUX_AUTH_REF", "google-default");
        std::env::set_var("GOOGLE_API_KEY", "google-key");
        std::env::set_var("OPENAI_API_KEY", "openai-key");
        std::env::remove_var("OPENAI_BASE_URL");

        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile(
                "google-default",
                "Google",
                "GOOGLE_API_KEY",
                Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            )
            .unwrap();
        store.save_default().unwrap();

        let client = OpenAIClient::from_env().unwrap();
        let config = client.config_clone();
        assert_eq!(config.api_key.as_deref(), Some("google-key"));
        assert_eq!(
            config.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );

        restore_env("KERUX_AUTH_STORE", old_auth_store);
        restore_env("KERUX_AUTH_REF", old_auth_ref);
        restore_env("GOOGLE_API_KEY", old_google_api_key);
        restore_env("OPENAI_API_KEY", old_openai_api_key);
        restore_env("OPENAI_BASE_URL", old_base_url);
        cleanup_auth_store_path(&auth_store_path);
    }

    #[test]
    #[serial]
    fn client_from_env_rejects_bearer_profile_without_base_url() {
        let auth_store_path = temp_auth_store_path("broken_bearer");
        cleanup_auth_store_path(&auth_store_path);
        let old_auth_store = std::env::var("KERUX_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("KERUX_AUTH_REF").ok();
        let old_bearer = std::env::var("GOOGLE_OAUTH_ACCESS_TOKEN").ok();
        let old_openai_api_key = std::env::var("OPENAI_API_KEY").ok();

        std::env::set_var("KERUX_AUTH_STORE", &auth_store_path);
        std::env::set_var("KERUX_AUTH_REF", "broken-bearer");
        std::env::set_var("GOOGLE_OAUTH_ACCESS_TOKEN", "google-token");
        std::env::set_var("OPENAI_API_KEY", "openai-key");

        let mut store = AuthStore::default();
        store.profiles.insert(
            "broken-bearer".to_string(),
            crate::auth::AuthProfile {
                provider: "google-gemini".to_string(),
                method: AuthMethod::BearerToken,
                base_url: None,
                secret_ref: "env:GOOGLE_OAUTH_ACCESS_TOKEN".to_string(),
                disabled: false,
                oauth: None,
            },
        );
        store.save_default().unwrap();

        let result = OpenAIClient::from_env();
        assert!(result.is_err());

        restore_env("KERUX_AUTH_STORE", old_auth_store);
        restore_env("KERUX_AUTH_REF", old_auth_ref);
        restore_env("GOOGLE_OAUTH_ACCESS_TOKEN", old_bearer);
        restore_env("OPENAI_API_KEY", old_openai_api_key);
        cleanup_auth_store_path(&auth_store_path);
    }

    #[test]
    #[serial]
    fn client_from_env_rejects_non_openai_api_key_profile_without_base_url() {
        let auth_store_path = temp_auth_store_path("google_api_key");
        cleanup_auth_store_path(&auth_store_path);
        let old_auth_store = std::env::var("KERUX_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("KERUX_AUTH_REF").ok();
        let old_google_api_key = std::env::var("GOOGLE_API_KEY").ok();
        let old_openai_api_key = std::env::var("OPENAI_API_KEY").ok();

        std::env::set_var("KERUX_AUTH_STORE", &auth_store_path);
        std::env::set_var("KERUX_AUTH_REF", "google-default");
        std::env::set_var("GOOGLE_API_KEY", "google-key");
        std::env::remove_var("OPENAI_API_KEY");

        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile("google-default", "Google", "GOOGLE_API_KEY", None)
            .unwrap();
        store.save_default().unwrap();

        let result = OpenAIClient::from_env();
        assert!(result.is_err());

        restore_env("KERUX_AUTH_STORE", old_auth_store);
        restore_env("KERUX_AUTH_REF", old_auth_ref);
        restore_env("GOOGLE_API_KEY", old_google_api_key);
        restore_env("OPENAI_API_KEY", old_openai_api_key);
        cleanup_auth_store_path(&auth_store_path);
    }

    #[test]
    fn anthropic_request_maps_system_tools_and_tool_results() {
        let client = AnthropicClient::new(ClientConfig {
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: Some("test-key".to_string()),
            timeout: Duration::from_secs(5),
            max_context_length: 200_000,
        })
        .unwrap();
        let tools = vec![crate::schema::ToolSchema {
            name: "file_read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let messages = vec![
            Message::system("Be precise."),
            Message::user("Read TODO.md"),
            Message::assistant("").with_tool_calls(vec![ToolCall {
                id: "call_1".to_string(),
                function: ToolCallFunction {
                    name: "file_read".to_string(),
                    arguments: r#"{"path":"TODO.md"}"#.to_string(),
                },
            }]),
            Message::tool("call_1", "contents"),
        ];

        let request = client
            .build_request("claude-sonnet", &messages, Some(&tools), true)
            .unwrap();

        assert_eq!(request["system"], "Be precise.");
        assert_eq!(request["stream"], true);
        assert_eq!(request["messages"][0]["role"], "user");
        assert_eq!(request["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(request["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(request["tools"][0]["name"], "file_read");
        assert!(request["tools"][0]["input_schema"].is_object());
        assert_eq!(
            client.messages_url().unwrap().as_str(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn anthropic_response_maps_text_tool_calls_and_usage() {
        let response = AnthropicClient::parse_anthropic_response(
            r#"{
                "id":"msg_1",
                "model":"claude-sonnet",
                "stop_reason":"tool_use",
                "usage":{"input_tokens":3,"output_tokens":4},
                "content":[
                    {"type":"text","text":"Reading file."},
                    {"type":"tool_use","id":"toolu_1","name":"file_read","input":{"path":"TODO.md"}}
                ]
            }"#,
            "claude-sonnet",
        )
        .unwrap();

        assert_eq!(response.usage.prompt_tokens, 3);
        assert_eq!(response.usage.completion_tokens, 4);
        assert_eq!(response.usage.total_tokens, 7);
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("Reading file.")
        );
        let tool_calls = response.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("tool calls should be mapped");
        assert_eq!(tool_calls[0].id.as_deref(), Some("toolu_1"));
        assert_eq!(
            tool_calls[0].function.as_ref().expect("function").arguments,
            "{\"path\":\"TODO.md\"}"
        );
    }

    #[test]
    fn anthropic_headers_require_api_key() {
        let client = AnthropicClient::new(ClientConfig {
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: None,
            timeout: Duration::from_secs(5),
            max_context_length: 200_000,
        })
        .unwrap();

        let error = client.build_headers().unwrap_err();
        assert!(error.to_string().contains("ANTHROPIC_API_KEY"));
    }

    fn restore_env(key: &str, value: Option<String>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }
}
