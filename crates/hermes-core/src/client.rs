//! OpenAI-compatible client with SSE streaming support
//!
//! A lightweight, custom implementation using `reqwest` and `serde`.
//! Supports Server-Sent Events for streaming responses.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use reqwest::{Client, header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE}};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, error, info, instrument};

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
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
            timeout: Duration::from_secs(60),
            max_context_length: 128_000,
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
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty());

        let base_url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        Ok(Self::new(ClientConfig {
            base_url,
            api_key,
            ..Default::default()
        }))
    }

    /// Build authorization headers
    fn build_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        
        // Content type
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

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
        
        let response = self.http_client
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
        
        let response = self.http_client
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
                request["tools"] = json!({ "tools": tools_array });
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
    pub index: usize,
    pub message: MessageDelta,
    pub finish_reason: Option<String>,
}

/// Message delta from API (non-streaming)
#[derive(Debug, Clone, Deserialize)]
pub struct MessageDelta {
    pub role: Option<Role>,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// Tool call delta
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallDelta {
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
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamingToolCallDelta>>,
}

/// Tool call delta for streaming
#[derive(Debug, Clone, Deserialize)]
pub struct StreamingToolCallDelta {
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
    pub fn new(stream: impl Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin + 'static) -> Self {
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
            // Try to find a complete SSE event in the buffer
            if let Some(event_end) = this.buffer.find("\n\n") {
                let event_data = this.buffer[..event_end].to_string();
                this.buffer = this.buffer[event_end + 2..].to_string();

                // Parse SSE event format: "data: {...}\n\n"
                for line in event_data.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            return Poll::Ready(None);
                        }

                        match serde_json::from_str::<ChatStreamEvent>(data.trim()) {
                            Ok(event) => return Poll::Ready(Some(Ok(event))),
                            Err(e) => {
                                // Try to recover by looking for partial JSON
                                if let Some(json_start) = data.find('{') {
                                    let potential_json = &data[json_start..];
                                    if let Ok(event) = serde_json::from_str::<ChatStreamEvent>(potential_json) {
                                        return Poll::Ready(Some(Ok(event)));
                                    }
                                }
                                // If parsing fails, continue to get more data
                                debug!(error = %e, "Failed to parse SSE event, will retry");
                            }
                        }
                    }
                }
                // Continue loop to look for more events
            } else {
                // No complete event, need to read more data
                break;
            }
        }

        // Poll the underlying stream for more data
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                // Append new data to buffer and try again
                if let Ok(text) = String::from_utf8(bytes.to_vec()) {
                    this.buffer.push_str(&text);
                }
                // Schedule wakeup and try to process
                cx.waker().wake_by_ref();
                Poll::Ready(Some(Ok(ChatStreamEvent {
                    id: String::new(),
                    object: String::new(),
                    created: 0,
                    model: String::new(),
                    choices: vec![],
                })))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(Error::Network(e)))),
            Poll::Ready(None) => {
                // Try to parse any remaining data in buffer
                if !this.buffer.is_empty() {
                    let remaining = this.buffer.clone();
                    this.buffer.clear();
                    if let Some(data) = remaining.strip_prefix("data: ") {
                        if data != "[DONE]" {
                            match serde_json::from_str::<ChatStreamEvent>(data.trim()) {
                                Ok(event) => return Poll::Ready(Some(Ok(event))),
                                Err(_) => return Poll::Ready(None),
                            }
                        }
                    }
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
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

    #[tokio::test]
    async fn test_client_from_env() {
        // This will succeed even without env vars (uses defaults)
        let client = OpenAIClient::from_env();
        assert!(client.is_ok());
    }
}
