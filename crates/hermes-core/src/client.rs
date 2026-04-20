//! OpenAI-compatible client with SSE streaming support
//!
//! A lightweight, custom implementation using reqwest and serde.
//! Supports Server-Sent Events for streaming responses.
//! Supports reasoning_content for extended-thinking models.

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
        let http_client = Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            http_client,
        }
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self> {
        let base = runtime_config();
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty());

        let base_url = std::env::var("OPENAI_BASE_URL").unwrap_or(base.client.base_url);

        Ok(Self::new(ClientConfig {
            base_url,
            api_key: api_key.or(base.client.api_key),
            timeout: Duration::from_secs(base.client.timeout_secs),
            max_context_length: base.client.max_context_length,
        }))
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
            return Err(Error::Agent(format!("HTTP {}: {}", status, body)));
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
            return Err(Error::Agent(format!("HTTP {}: {}", status, body)));
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
#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// SSE streaming event from the OpenAI API
#[derive(Debug, Clone, Deserialize)]
pub struct ChatStreamEvent {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
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
}

impl ChatStreamResponse {
    pub fn new(
        stream: impl Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
    ) -> Self {
        Self {
            inner: Box::new(stream),
            buffer: String::new(),
        }
    }
}

impl Stream for ChatStreamResponse {
    type Item = crate::error::Result<ChatStreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            if let Some(event) = try_parse_next_sse_event(&mut this.buffer, false) {
                return Poll::Ready(Some(Ok(event)));
            }

            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                        this.buffer.push_str(&text);
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(Error::Network(e)))),
                Poll::Ready(None) => {
                    return Poll::Ready(try_parse_next_sse_event(&mut this.buffer, true).map(Ok));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn try_parse_next_sse_event(buffer: &mut String, allow_partial: bool) -> Option<ChatStreamEvent> {
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

    let payload = event_data
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");

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
            debug!(error = %e, payload = %payload, "Failed to parse SSE event");
            None
        }
    }
}

fn normalize_sse_buffer(buffer: &mut String) {
    if buffer.contains('\r') {
        *buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
    }
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
        let event = try_parse_next_sse_event(&mut buffer, false).expect("event should parse");

        assert_eq!(event.choices.len(), 1);
        assert_eq!(event.choices[0].delta.content.as_deref(), Some("Hello"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn streaming_parser_handles_partial_final_event() {
        let mut buffer = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"demo\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}]}".to_string();
        let event =
            try_parse_next_sse_event(&mut buffer, true).expect("trailing event should parse");

        assert_eq!(event.choices[0].delta.content.as_deref(), Some("Done"));
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    async fn test_client_from_env() {
        // This will succeed even without env vars (uses defaults)
        let client = OpenAIClient::from_env();
        assert!(client.is_ok());
    }
}
