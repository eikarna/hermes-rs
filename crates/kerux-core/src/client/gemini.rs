//! Google Gemini adapter: `generateContent` / `streamGenerateContent?alt=sse`.
//!
//! **ponytail:** streaming is one-shot — the whole non-streaming response is
//! replayed as a single OpenAI-shaped SSE event so the shared SSE parser in
//! `ChatStreamResponse` can be reused unchanged. Upgrade path: map Gemini's
//! chunked `candidates[].content.parts[]` deltas onto `ChatStreamEvent` for
//! true token streaming (same shape as Anthropic's SSE normalization).

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::client::{
    ChatResponse, ChatStreamResponse, Choice, ClientConfig, EditFormat, LLMProvider, Message,
    MessageDelta, ProviderCapabilities, Role, ToolCall, ToolCallDelta, ToolCallFunction, Usage,
};
use crate::error::{Error, Result};
use crate::schema::ToolSchema;

/// Gemini adapter. Base URL should end at the API prefix
/// (e.g. `https://generativelanguage.googleapis.com/v1beta`).
#[derive(Debug, Clone)]
pub struct GeminiClient {
    config: ClientConfig,
    http_client: reqwest::Client,
}

impl GeminiClient {
    pub fn new(config: ClientConfig) -> Result<Self> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(config.timeout)
            .build()
            .map_err(|e| Error::Config(format!("Failed to create HTTP client: {}", e)))?;
        Ok(Self {
            config,
            http_client,
        })
    }

    fn api_key(&self) -> Result<&str> {
        self.config.api_key.as_deref().ok_or_else(|| {
            Error::Config(
                "Gemini provider requires GEMINI_API_KEY or [client.gemini].api_key".into(),
            )
        })
    }

    fn endpoint(&self, model: &str, action: &str) -> Result<String> {
        let base = self.config.base_url.trim_end_matches('/');
        Ok(format!(
            "{}/models/{}:{}?key={}",
            base,
            model,
            action,
            self.api_key()?
        ))
    }

    /// OpenAI-shaped message list → Gemini `contents` + `systemInstruction`.
    /// `assistant` becomes `model`; system lines fold into `systemInstruction`;
    /// tool results fold into a `functionResponse` part under `user`.
    fn build_request(messages: &[Message], tools: Option<&[ToolSchema]>) -> Value {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for m in messages {
            match m.role {
                Role::System => system_parts.push(m.content.trim()),
                Role::User => contents.push(json!({
                    "role": "user",
                    "parts": [{ "text": m.content }],
                })),
                Role::Assistant => {
                    let mut parts: Vec<Value> = Vec::new();
                    if !m.content.trim().is_empty() {
                        parts.push(json!({ "text": m.content }));
                    }
                    if let Some(calls) = m.tool_calls.as_deref() {
                        for call in calls {
                            parts.push(json!({
                                "functionCall": {
                                    "name": call.function.name,
                                    "args": serde_json::from_str::<Value>(&call.function.arguments)
                                        .unwrap_or_else(|_| json!({})),
                                }
                            }));
                        }
                    }
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
                Role::Tool => contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": m.tool_call_id.clone().unwrap_or_default(),
                            "response": { "content": m.content },
                        }
                    }],
                })),
            }
        }

        let mut request = json!({ "contents": contents });
        if !system_parts.is_empty() {
            request["systemInstruction"] = json!({
                "parts": [{ "text": system_parts.join("\n\n") }],
            });
        }
        if let Some(tools) = tools.filter(|t| !t.is_empty()) {
            request["tools"] = json!([{
                "functionDeclarations": tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })).collect::<Vec<_>>(),
            }]);
        }
        request
    }

    pub async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        let request = Self::build_request(messages, tools);
        let response = self
            .http_client
            .post(self.endpoint(model, "generateContent")?)
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
        Self::parse_response(&body, model)
    }

    /// Gemini → OpenAI-shaped `ChatResponse` so the agent loop is provider-
    /// agnostic. `functionCall` parts map onto `tool_calls` with `index`-based
    /// IDs (Gemini has no call IDs).
    fn parse_response(body: &str, model: &str) -> Result<ChatResponse> {
        let value: Value = serde_json::from_str(body)
            .map_err(|e| Error::ParseResponse(format!("Invalid Gemini response: {}", e)))?;

        let usage = value
            .get("usageMetadata")
            .map(|u| Usage {
                prompt_tokens: u
                    .get("promptTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                completion_tokens: u
                    .get("candidatesTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                total_tokens: u
                    .get("totalTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                cached_prompt_tokens: u
                    .get("cachedContentTokenCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
            })
            .unwrap_or_default();

        let candidate = value
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|c| c.first());
        let finish_reason = candidate
            .and_then(|c| c.get("finishReason"))
            .and_then(Value::as_str)
            .map(|r| match r {
                "STOP" | "MAX_TOKENS" => "stop".to_string(),
                other => other.to_ascii_lowercase(),
            });

        let parts = candidate
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for part in &parts {
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                text.push_str(t);
            }
            if let Some(fc) = part.get("functionCall") {
                if let Some(name) = fc.get("name").and_then(Value::as_str) {
                    let arguments = match fc.get("args") {
                        Some(args) => serde_json::to_string(args).unwrap_or_default(),
                        None => "{}".to_string(),
                    };
                    tool_calls.push(ToolCall {
                        id: format!("gemini-call-{}", tool_calls.len()),
                        function: ToolCallFunction {
                            name: name.to_string(),
                            arguments,
                        },
                    });
                }
            }
        }

        let mut message = MessageDelta {
            role: Some(Role::Assistant),
            content: if text.is_empty() { None } else { Some(text) },
            reasoning_content: None,
            tool_calls: None,
        };
        if !tool_calls.is_empty() {
            message.tool_calls = Some(
                tool_calls
                    .into_iter()
                    .map(|tc| ToolCallDelta {
                        index: 0,
                        id: Some(tc.id),
                        call_type: Some("function".to_string()),
                        function: Some(tc.function),
                    })
                    .collect(),
            );
        }

        Ok(ChatResponse {
            id: format!("gemini-{}", std::process::id()),
            object: "chat.completion".to_string(),
            created: 0,
            model: model.to_string(),
            choices: vec![Choice {
                index: 0,
                message,
                finish_reason,
            }],
            usage,
        })
    }

    /// One-shot streaming: performs a non-streaming call, converts the result
    /// into a single OpenAI-shaped SSE chunk, and feeds it through the shared
    /// `ChatStreamResponse` parser. See module-level `ponytail` note.
    pub async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        let request = Self::build_request(messages, tools);
        let response = self
            .http_client
            .post(self.endpoint(model, "streamGenerateContent")?)
            .query(&[("alt", "sse")])
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
        // Gemini's SSE frames contain the same JSON as generateContent; the
        // shared SSE parser only understands OpenAI chunks, so wrap the raw
        // byte stream in a normalizer that emits OpenAI-shaped events.
        let stream = GeminiToOpenAiStream::new(response.bytes_stream(), model.to_string());
        Ok(ChatStreamResponse::new(stream))
    }
}

/// Byte-stream adapter: parse Gemini SSE frames into OpenAI-shaped
/// `chat.completion.chunk` JSON, then re-encode as SSE so `ChatStreamResponse`
/// parses it with zero changes.
struct GeminiToOpenAiStream {
    inner: std::pin::Pin<
        Box<dyn futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send>,
    >,
    buffer: String,
    model: String,
    done: bool,
}

impl GeminiToOpenAiStream {
    fn new(
        inner: impl futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>>
            + Send
            + Unpin
            + 'static,
        model: String,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            buffer: String::new(),
            model,
            done: false,
        }
    }
}

impl futures::Stream for GeminiToOpenAiStream {
    type Item = std::result::Result<bytes::Bytes, reqwest::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        let this = self.get_mut();
        loop {
            // Emit any normalized frames already produced.
            if let Some(pos) = this.buffer.find("\n\n") {
                let frame = this.buffer.drain(..pos + 2).collect::<String>();
                return Poll::Ready(Some(Ok(bytes::Bytes::from(frame))));
            }
            if this.done {
                return Poll::Ready(None);
            }
            match std::pin::Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.done = true;
                    if this.buffer.is_empty() {
                        return Poll::Ready(None);
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok(bytes))) => {
                    let chunk = String::from_utf8_lossy(&bytes);
                    for block in chunk.split("\n\n") {
                        let data = block
                            .lines()
                            .filter_map(|l| l.strip_prefix("data:"))
                            .map(str::trim)
                            .collect::<Vec<_>>()
                            .join("");
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        if let Some(frame) = gemini_frame_to_openai(&data, &this.model) {
                            this.buffer.push_str(&format!("data: {}\n\n", frame));
                        }
                    }
                }
            }
        }
    }
}

/// Convert one Gemini `streamGenerateContent` JSON frame into an OpenAI
/// `chat.completion.chunk` JSON string. Text and functionCall parts are both
/// mapped; failures return `None` (frame dropped).
fn gemini_frame_to_openai(data: &str, model: &str) -> Option<String> {
    let value: Value = serde_json::from_str(data).ok()?;
    let candidate = value.get("candidates")?.as_array()?.first()?;
    let parts: &[Value] = candidate.get("content")?.get("parts")?.as_array()?;

    let mut text = String::new();
    let mut tool_call_json: Vec<Value> = Vec::new();
    for part in parts {
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            text.push_str(t);
        }
        if let Some(fc) = part.get("functionCall") {
            if let Some(name) = fc.get("name").and_then(Value::as_str) {
                let arguments = fc
                    .get("args")
                    .map(serde_json::to_string)
                    .and_then(std::result::Result::ok)
                    .unwrap_or_else(|| "{}".to_string());
                tool_call_json.push(json!({
                    "index": tool_call_json.len(),
                    "id": format!("gemini-call-{}", tool_call_json.len()),
                    "type": "function",
                    "function": { "name": name, "arguments": arguments },
                }));
            }
        }
    }

    let mut delta = json!({ "role": "assistant" });
    if !text.is_empty() {
        delta["content"] = json!(text);
    }
    if !tool_call_json.is_empty() {
        delta["tool_calls"] = json!(tool_call_json);
    }
    let chunk = json!({
        "id": "gemini-stream",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": null }],
    });
    serde_json::to_string(&chunk).ok()
}

#[async_trait]
impl LLMProvider for GeminiClient {
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
        if let Some(table) = crate::client::provider::lookup_capabilities(model) {
            return table;
        }
        ProviderCapabilities {
            max_input_tokens: self.config.max_context_length,
            max_output_tokens: 8_192,
            edit_format: EditFormat::Patch,
            supports_streaming: true,
            supports_reasoning: false,
            supports_vision: true,
            supports_tool_calls: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::FallbackChainProvider;
    use std::time::Duration;

    fn tool_schema() -> ToolSchema {
        ToolSchema::new("get_weather", "Get weather", json!({"type": "object"}))
    }

    #[test]
    fn request_shape_uses_gemini_vocabulary() {
        let messages = vec![
            Message::system("be terse"),
            Message::user("hi"),
            Message::assistant("hello"),
            Message::tool("call-1", "sunny"),
        ];
        let req = GeminiClient::build_request(&messages, Some(&[tool_schema()]));
        assert_eq!(
            req["systemInstruction"]["parts"][0]["text"],
            Value::String("be terse".into())
        );
        assert_eq!(req["contents"][0]["role"], "user");
        assert_eq!(req["contents"][1]["role"], "model");
        assert_eq!(
            req["contents"][2]["parts"][0]["functionResponse"]["name"],
            "call-1"
        );
        assert_eq!(
            req["tools"][0]["functionDeclarations"][0]["name"],
            "get_weather"
        );
    }

    #[test]
    fn parse_text_and_function_call() {
        let body = r#"{
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [
                    {"text": "let me check"},
                    {"functionCall": {"name": "get_weather", "args": {"city": "sf"}}}
                ]}
            }],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15}
        }"#;
        let resp = GeminiClient::parse_response(body, "gemini-2.5-flash").unwrap();
        assert_eq!(resp.usage.total_tokens, 15);
        let msg = &resp.choices[0].message;
        assert_eq!(msg.content.as_deref(), Some("let me check"));
        let tc = &msg.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.function.as_ref().unwrap().name, "get_weather");
        assert!(tc.function.as_ref().unwrap().arguments.contains("sf"));
    }

    #[test]
    fn gemini_frame_maps_to_openai_chunk() {
        let frame = r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}"#;
        let out = gemini_frame_to_openai(frame, "gemini-2.5-flash").unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["choices"][0]["delta"]["content"], "hi");
    }

    #[test]
    fn capabilities_default_for_unknown_gemini() {
        let client = GeminiClient::new(ClientConfig::default()).unwrap();
        let caps = client.capabilities("gemini-9.9-future");
        assert!(caps.supports_vision);
        assert_eq!(caps.edit_format, EditFormat::Patch);
    }

    #[test]
    fn capabilities_table_hits_2_5() {
        let client = GeminiClient::new(ClientConfig::default()).unwrap();
        let caps = client.capabilities("gemini-2.5-pro");
        assert_eq!(caps.max_output_tokens, 65_536);
        assert_eq!(caps.edit_format, EditFormat::SearchReplace);
        assert!(caps.supports_reasoning);
    }

    // -- HTTP error typing (fallback integration) ---------------------------

    fn test_client(url: String) -> GeminiClient {
        GeminiClient::new(ClientConfig {
            base_url: url,
            api_key: Some("test-key-redacted".into()),
            timeout: Duration::from_secs(5),
            max_context_length: 128_000,
        })
        .unwrap()
    }

    fn messages() -> Vec<Message> {
        vec![Message::user("hi")]
    }

    /// 429 must surface as typed `Error::Http` and be classified
    /// fallback-worthy so the chain retries the next provider.
    #[tokio::test]
    async fn chat_rate_limit_is_typed_and_fallback_worthy() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock(
                "POST",
                mockito::Matcher::Regex(r"/models/.*generateContent".into()),
            )
            .with_status(429)
            .with_body(r#"{"error":{"code":429,"message":"quota exceeded"}}"#)
            .create_async()
            .await;

        let client = test_client(server.url());
        let err = client
            .chat("gemini-2.5-flash", &messages(), None)
            .await
            .unwrap_err();

        match &err {
            Error::Http { status, body } => {
                assert_eq!(*status, 429);
                assert!(body.contains("quota"));
            }
            other => panic!("expected Error::Http, got: {other:?}"),
        }
        assert!(FallbackChainProvider::is_fallback_worthy(&err));
    }

    /// Streaming 5xx must also be typed so an upstream outage triggers
    /// fallback instead of bubbling as an opaque agent error.
    #[tokio::test]
    async fn chat_streaming_server_error_is_typed_and_fallback_worthy() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock(
                "POST",
                mockito::Matcher::Regex(r"/models/.*streamGenerateContent".into()),
            )
            .with_status(503)
            .with_body("upstream unavailable")
            .create_async()
            .await;

        let client = test_client(server.url());
        // No unwrap_err(): ChatStreamResponse doesn't implement Debug.
        let err = match client
            .chat_streaming("gemini-2.5-flash", &messages(), None)
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("expected chat_streaming to fail with 503"),
        };

        match &err {
            Error::Http { status, body } => {
                assert_eq!(*status, 503);
                assert!(body.contains("upstream"));
            }
            other => panic!("expected Error::Http, got: {other:?}"),
        }
        assert!(FallbackChainProvider::is_fallback_worthy(&err));
    }

    /// Deterministic failures stay non-fallback-worthy: a bad key would
    /// fail identically on every provider, so the chain returns it as-is.
    #[tokio::test]
    async fn chat_auth_error_is_not_fallback_worthy() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock(
                "POST",
                mockito::Matcher::Regex(r"/models/.*generateContent".into()),
            )
            .with_status(401)
            .with_body(r#"{"error":{"code":401,"message":"invalid key"}}"#)
            .create_async()
            .await;

        let client = test_client(server.url());
        let err = client
            .chat("gemini-2.5-flash", &messages(), None)
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Http { status: 401, .. }));
        assert!(!FallbackChainProvider::is_fallback_worthy(&err));
    }

    /// Happy path unchanged by the error-typing refactor.
    #[tokio::test]
    async fn chat_success_parses_gemini_response() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", mockito::Matcher::Regex(r"/models/.*generateContent".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "candidates": [{
                        "finishReason": "STOP",
                        "content": {"parts": [{"text": "pong"}]}
                    }],
                    "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2}
                }"#,
            )
            .create_async()
            .await;

        let client = test_client(server.url());
        let resp = client
            .chat("gemini-2.5-flash", &messages(), None)
            .await
            .unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("pong"));
        assert_eq!(resp.usage.total_tokens, 2);
    }
}
