//! Opt-in live capability probes for models the user selects in the wizard.
//!
//! Catalog metadata and name heuristics (see [`crate::capability`]) cover most
//! models, but providers without metadata leave verdicts as guesses. For a
//! model the user explicitly picks, the wizard can run up to three cheap live
//! probes to verify streaming, tool-calling, and vision support.
//!
//! Probes are opt-in and per-model — never run against a whole model list.
//! Embeddings, rerank, image generation, and video generation are NOT probed
//! (different endpoints, real cost); those stay on metadata/heuristics.
//!
//! Verdict semantics: `None` = untested or inconclusive (network failure,
//! timeout, transient provider error); `Some(false)` = the provider actively
//! rejected the capability; `Some(true)` = verified live.

use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::json;

use crate::capability::{Capability, CapabilityStatus};
use crate::client::{ImageContent, LLMProvider, Message};
use crate::error::Error;
use crate::schema::ToolSchema;

/// Default per-probe timeout.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// A 1x1 transparent PNG (base64) — the smallest valid vision payload.
pub const PROBE_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// Outcome of the live probe suite for one model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeResult {
    /// SSE chunks flowed (`Some(true)`), the provider refused (`Some(false)`),
    /// or the probe was inconclusive (`None`).
    pub streaming: Option<bool>,
    /// A tool definition was accepted and the model emitted a tool call.
    pub tools: Option<bool>,
    /// An image payload was accepted by the model.
    pub vision: Option<bool>,
    /// Time-to-first-token of the streaming probe, in milliseconds.
    pub ttft_ms: Option<u64>,
}

impl ProbeResult {
    /// Map verified verdicts to capability updates for
    /// [`crate::capability::CapabilityReport::merge_probe`]. Inconclusive
    /// (`None`) results are omitted so earlier verdicts are preserved.
    pub fn to_capability_updates(&self) -> Vec<(Capability, CapabilityStatus)> {
        let mut updates = Vec::new();
        for (cap, verdict) in [
            (Capability::Streaming, self.streaming),
            (Capability::Tools, self.tools),
            (Capability::Vision, self.vision),
        ] {
            if let Some(ok) = verdict {
                updates.push((
                    cap,
                    if ok {
                        CapabilityStatus::Supported
                    } else {
                        CapabilityStatus::Unsupported
                    },
                ));
            }
        }
        updates
    }
}

/// Run all three probes sequentially against one model. Each probe gets its
/// own `timeout` and fails gracefully — one failing probe never blocks the
/// others.
pub async fn probe_model(
    provider: &dyn LLMProvider,
    model: &str,
    timeout: Duration,
) -> ProbeResult {
    let mut result = ProbeResult::default();

    let (streaming, ttft) = probe_streaming(provider, model, timeout).await;
    result.streaming = streaming;
    result.ttft_ms = ttft;
    result.tools = probe_tools(provider, model, timeout).await;
    result.vision = probe_vision(provider, model, timeout).await;

    result
}

/// Streaming probe: send a mini completion ("Say hi"), verify SSE chunks
/// actually flow, and measure time-to-first-token.
pub async fn probe_streaming(
    provider: &dyn LLMProvider,
    model: &str,
    timeout: Duration,
) -> (Option<bool>, Option<u64>) {
    let messages = [Message::user("Say hi")];
    let started = Instant::now();
    let outcome = tokio::time::timeout(timeout, async {
        let mut stream = provider.chat_streaming(model, &messages, None).await?;
        while let Some(event) = stream.next().await {
            if event.is_ok() {
                return Ok(true);
            }
        }
        // Connected, but no parseable chunk ever flowed.
        Ok(false)
    })
    .await;

    match outcome {
        Ok(Ok(true)) => (
            Some(true),
            Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        ),
        Ok(Ok(false)) => (Some(false), None),
        Ok(Err(err)) => {
            if is_deterministic_client_error(&err) {
                // The request was plain "Say hi" with no tools or images, so
                // a 4xx means the provider rejected streaming (or the model).
                (Some(false), None)
            } else {
                (None, None)
            }
        }
        Err(_) => (None, None),
    }
}

/// Tool-calling probe: send a request carrying one trivial tool definition
/// (`get_time`) and check the response contains a tool call.
pub async fn probe_tools(
    provider: &dyn LLMProvider,
    model: &str,
    timeout: Duration,
) -> Option<bool> {
    let messages = [Message::user(
        "What time is it right now? Answer by calling the tool.",
    )];
    let tool = ToolSchema::new(
        "get_time",
        "Returns the current date and time",
        json!({"type": "object", "properties": {}, "additionalProperties": false}),
    );
    let tools = [tool];

    let outcome =
        tokio::time::timeout(timeout, provider.chat(model, &messages, Some(&tools))).await;
    match outcome {
        Ok(Ok(response)) => {
            let called = response
                .choices
                .first()
                .and_then(|choice| choice.message.tool_calls.as_deref())
                .is_some_and(|calls| !calls.is_empty());
            Some(called)
        }
        Ok(Err(err)) => match &err {
            Error::Http { status, body } if is_client_error_status(*status) => {
                mentions_tools(body).then_some(false)
            }
            _ => None,
        },
        Err(_) => None,
    }
}

/// Vision probe: send a 1px base64 PNG in message content and check the
/// provider does not reject the modality.
pub async fn probe_vision(
    provider: &dyn LLMProvider,
    model: &str,
    timeout: Duration,
) -> Option<bool> {
    let messages = [
        Message::user("Describe this image.").with_images(vec![ImageContent {
            media_type: "image/png".to_string(),
            data_base64: PROBE_PNG_BASE64.to_string(),
        }]),
    ];

    let outcome = tokio::time::timeout(timeout, provider.chat(model, &messages, None)).await;
    match outcome {
        Ok(Ok(_)) => Some(true),
        Ok(Err(err)) => match &err {
            Error::Http { status, body } if is_client_error_status(*status) => {
                mentions_vision(body).then_some(false)
            }
            _ => None,
        },
        Err(_) => None,
    }
}

/// 4xx (except 429) — deterministic client errors that are not transient.
fn is_client_error_status(status: u16) -> bool {
    status != 429 && (400..500).contains(&status)
}

fn is_deterministic_client_error(err: &Error) -> bool {
    matches!(err, Error::Http { status, .. } if is_client_error_status(*status))
}

fn mentions_tools(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    ["tool", "function_call", "function calling"]
        .iter()
        .any(|needle| body.contains(needle))
}

fn mentions_vision(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    [
        "image",
        "vision",
        "modality",
        "multimodal",
        "media type",
        "media_type",
        "base64",
    ]
    .iter()
    .any(|needle| body.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientConfig, OpenAIClient};
    use mockito::Matcher;

    fn client_for(base_url: &str) -> OpenAIClient {
        OpenAIClient::new(ClientConfig {
            base_url: base_url.to_string(),
            api_key: Some("test-key".to_string()),
            timeout: Duration::from_secs(5),
            max_context_length: 8_192,
        })
    }

    const STREAM_OK_BODY: &str = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );

    const TOOL_CALL_BODY: &str = r#"{"id":"c2","object":"chat.completion","created":1,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_time","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

    const TEXT_BODY: &str = r#"{"id":"c3","object":"chat.completion","created":1,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"It is noon."},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

    #[tokio::test]
    async fn streaming_probe_verifies_chunks_and_ttft() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::Regex("\"stream\":true".into()))
            .with_header("content-type", "text/event-stream")
            .with_body(STREAM_OK_BODY)
            .create_async()
            .await;
        let client = client_for(&server.url());

        let (verdict, ttft) = probe_streaming(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, Some(true));
        assert!(ttft.is_some());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn streaming_probe_reports_provider_rejection() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_body("{\"error\":\"streaming is not supported\"}")
            .create_async()
            .await;
        let client = client_for(&server.url());

        let (verdict, ttft) = probe_streaming(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, Some(false));
        assert!(ttft.is_none());
    }

    #[tokio::test]
    async fn streaming_probe_times_out_to_none() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(|w| {
                std::thread::sleep(Duration::from_secs(2));
                w.write_all(b"data: [DONE]\n\n")
            })
            .create_async()
            .await;
        let client = client_for(&server.url());

        let (verdict, ttft) =
            probe_streaming(&client, "probe-model", Duration::from_millis(300)).await;

        assert_eq!(verdict, None);
        assert!(ttft.is_none());
    }

    #[tokio::test]
    async fn tools_probe_verifies_tool_call() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::Regex("get_time".into()))
            .with_body(TOOL_CALL_BODY)
            .create_async()
            .await;
        let client = client_for(&server.url());

        let verdict = probe_tools(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, Some(true));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn tools_probe_false_when_model_answers_in_text() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_body(TEXT_BODY)
            .create_async()
            .await;
        let client = client_for(&server.url());

        let verdict = probe_tools(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, Some(false));
    }

    #[tokio::test]
    async fn tools_probe_false_on_tool_rejection() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_body("{\"error\":\"tools are not supported by this model\"}")
            .create_async()
            .await;
        let client = client_for(&server.url());

        let verdict = probe_tools(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, Some(false));
    }

    #[tokio::test]
    async fn vision_probe_verifies_image_payload() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::Regex("image_url".into()))
            .with_body(TEXT_BODY)
            .create_async()
            .await;
        let client = client_for(&server.url());

        let verdict = probe_vision(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, Some(true));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn vision_probe_false_on_modality_rejection() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_body("{\"error\":\"this model does not support image input\"}")
            .create_async()
            .await;
        let client = client_for(&server.url());

        let verdict = probe_vision(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, Some(false));
    }

    #[tokio::test]
    async fn vision_probe_none_on_transient_error() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/chat/completions")
            .with_status(429)
            .with_body("{\"error\":\"rate limited\"}")
            .create_async()
            .await;
        let client = client_for(&server.url());

        let verdict = probe_vision(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(verdict, None);
    }

    #[tokio::test]
    async fn network_failure_yields_none() {
        // Port 9 (discard) refuses connections on localhost.
        let client = client_for("http://127.0.0.1:9");

        assert_eq!(
            probe_tools(&client, "probe-model", Duration::from_secs(5)).await,
            None
        );
        assert_eq!(
            probe_vision(&client, "probe-model", Duration::from_secs(5)).await,
            None
        );
        let (verdict, ttft) = probe_streaming(&client, "probe-model", Duration::from_secs(5)).await;
        assert_eq!(verdict, None);
        assert!(ttft.is_none());
    }

    #[tokio::test]
    async fn probe_model_runs_all_probes() {
        let mut server = mockito::Server::new_async().await;
        let stream_mock = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::Regex("\"stream\":true".into()))
            .with_header("content-type", "text/event-stream")
            .with_body(STREAM_OK_BODY)
            .create_async()
            .await;
        let tools_mock = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::Regex("get_time".into()))
            .with_body(TOOL_CALL_BODY)
            .create_async()
            .await;
        let vision_mock = server
            .mock("POST", "/chat/completions")
            .match_body(Matcher::Regex("image_url".into()))
            .with_body(TEXT_BODY)
            .create_async()
            .await;
        let client = client_for(&server.url());

        let result = probe_model(&client, "probe-model", Duration::from_secs(5)).await;

        assert_eq!(result.streaming, Some(true));
        assert_eq!(result.tools, Some(true));
        assert_eq!(result.vision, Some(true));
        assert!(result.ttft_ms.is_some());
        stream_mock.assert_async().await;
        tools_mock.assert_async().await;
        vision_mock.assert_async().await;
    }

    #[test]
    fn probe_result_maps_verdicts_to_capability_updates() {
        let result = ProbeResult {
            streaming: Some(true),
            tools: Some(false),
            vision: None,
            ttft_ms: Some(42),
        };
        let updates = result.to_capability_updates();

        assert_eq!(
            updates,
            vec![
                (Capability::Streaming, CapabilityStatus::Supported),
                (Capability::Tools, CapabilityStatus::Unsupported),
            ]
        );
    }
}
