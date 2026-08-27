//! Soak- and integration tests for the fallback provider chain (F3).
//!
//! These tests exercise [`FallbackChainProvider`] against real HTTP servers
//! (mockito) rather than scripted in-memory providers, so the full path —
//! adapter request → typed HTTP error → fallback classification → next
//! provider — is covered end to end.
//!
//! Coverage:
//! - cross-provider fallthrough (OpenAI ↔ Anthropic ↔ Gemini) on 429/5xx
//! - streaming fallthrough with SSE drain verification
//! - deterministic errors (401) never leave the primary
//! - per-fallback model overrides reach the wire
//! - chain exhaustion returns the last fallback's error
//! - network-level failures (connection refused) trigger fallback
//! - soak: hundreds of sequential iterations and a concurrent burst,
//!   verifying graceful degradation stays stable under sustained failure.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kerux_core::client::gemini::GeminiClient;
use kerux_core::client::{
    AnthropicClient, ClientConfig, FallbackChainProvider, LLMProvider, Message, OpenAIClient,
};
use kerux_core::error::Error;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn client_config(base_url: String) -> ClientConfig {
    ClientConfig {
        base_url,
        api_key: Some("test-key-redacted".into()),
        timeout: Duration::from_secs(5),
        max_context_length: 128_000,
    }
}

fn openai_provider(base_url: String) -> Arc<dyn LLMProvider> {
    Arc::new(OpenAIClient::new(client_config(base_url)))
}

fn anthropic_provider(base_url: String) -> Arc<dyn LLMProvider> {
    Arc::new(AnthropicClient::new(client_config(base_url)).unwrap())
}

fn gemini_provider(base_url: String) -> Arc<dyn LLMProvider> {
    Arc::new(GeminiClient::new(client_config(base_url)).unwrap())
}

fn messages() -> Vec<Message> {
    vec![Message::user("ping")]
}

fn openai_ok_body(text: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-1","object":"chat.completion","created":0,"model":"m","choices":[{{"index":0,"message":{{"role":"assistant","content":"{text}"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}}}"#
    )
}

fn anthropic_ok_body(text: &str) -> String {
    format!(
        r#"{{"id":"msg_1","model":"claude-sonnet","stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":1}},"content":[{{"type":"text","text":"{text}"}}]}}"#
    )
}

fn openai_sse_body(text: &str) -> String {
    format!(
        "data: {{\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: [DONE]\n\n"
    )
}

async fn drain_stream_text(mut stream: kerux_core::client::ChatStreamResponse) -> String {
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("stream event should parse");
        if let Some(chunk) = event
            .choices
            .first()
            .and_then(|c| c.delta.content.as_deref())
        {
            text.push_str(chunk);
        }
    }
    text
}

// ---------------------------------------------------------------------------
// Cross-provider integration
// ---------------------------------------------------------------------------

/// OpenAI primary rate-limited → Anthropic fallback serves the response.
#[tokio::test]
async fn openai_429_falls_back_to_anthropic() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body(r#"{"error":{"message":"rate limited"}}"#)
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(anthropic_ok_body("anthropic-ok"))
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(anthropic_provider(fallback_srv.url()), None)],
    );

    let resp = chain.chat("m", &messages(), None).await.unwrap();
    assert_eq!(
        resp.choices[0].message.content.as_deref(),
        Some("anthropic-ok")
    );
}

/// Anthropic primary outage (typed 503) → OpenAI fallback serves the
/// response. Guards the Anthropic adapter's typed-error contract: a 5xx
/// must be classified fallback-worthy, not bubble as `Error::Agent`.
#[tokio::test]
async fn anthropic_503_falls_back_to_openai() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/messages")
        .with_status(503)
        .with_body("overloaded")
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok_body("openai-ok"))
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        anthropic_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    let resp = chain
        .chat("claude-sonnet", &messages(), None)
        .await
        .unwrap();
    assert_eq!(
        resp.choices[0].message.content.as_deref(),
        Some("openai-ok")
    );
}

/// Gemini primary rate-limited → OpenAI fallback. Gemini's `?key=` auth
/// path must not interfere with fallback routing.
#[tokio::test]
async fn gemini_429_falls_back_to_openai() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock(
            "POST",
            mockito::Matcher::Regex(r"/models/.*generateContent".into()),
        )
        .with_status(429)
        .with_body(r#"{"error":{"code":429,"message":"quota"}}"#)
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok_body("openai-ok"))
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        gemini_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    let resp = chain
        .chat("gemini-2.5-flash", &messages(), None)
        .await
        .unwrap();
    assert_eq!(
        resp.choices[0].message.content.as_deref(),
        Some("openai-ok")
    );
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Streaming primary 429 → fallback stream is drained to completion.
#[tokio::test]
async fn streaming_429_falls_back_and_drains() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body("slow down")
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(openai_sse_body("stream-ok"))
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    let stream = chain
        .chat_streaming("m", &messages(), None)
        .await
        .expect("streaming should succeed via fallback");
    assert_eq!(drain_stream_text(stream).await, "stream-ok");
}

/// Streaming primary success → fallback receives zero traffic.
#[tokio::test]
async fn streaming_primary_success_skips_fallback() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(openai_sse_body("primary-stream"))
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_body(openai_ok_body("never"))
        .expect(0)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    let stream = chain
        .chat_streaming("m", &messages(), None)
        .await
        .expect("primary streaming should succeed");
    assert_eq!(drain_stream_text(stream).await, "primary-stream");
}

// ---------------------------------------------------------------------------
// Classification edge cases
// ---------------------------------------------------------------------------

/// Deterministic 401 on the primary must NOT fall back: the same bad key
/// would fail identically everywhere and a silent switch hides the problem.
#[tokio::test]
async fn deterministic_401_never_falls_back() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(401)
        .with_body(r#"{"error":{"message":"invalid key"}}"#)
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_body(openai_ok_body("never"))
        .expect(0)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    let err = chain.chat("m", &messages(), None).await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 401, .. }));
}

/// When the primary fails transiently but the fallback fails
/// deterministically, the fallback's error is what surfaces (last error
/// wins), not the primary's.
#[tokio::test]
async fn fallback_deterministic_error_surfaces_after_transient_primary() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body("limited")
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(401)
        .with_body("fallback key invalid")
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    let err = chain.chat("m", &messages(), None).await.unwrap_err();
    match err {
        Error::Http { status, body } => {
            assert_eq!(status, 401);
            assert!(body.contains("fallback key invalid"));
        }
        other => panic!("expected Error::Http, got: {other:?}"),
    }
}

/// Network-level failure (connection refused) is fallback-worthy.
#[tokio::test]
async fn network_error_falls_back() {
    // Bind then immediately drop a listener to get a port that refuses
    // connections deterministically.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = listener.local_addr().unwrap().port();
    drop(listener);

    let mut fallback_srv = mockito::Server::new_async().await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok_body("recovered"))
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(format!("http://127.0.0.1:{dead_port}/v1")),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    let resp = chain.chat("m", &messages(), None).await.unwrap();
    assert_eq!(
        resp.choices[0].message.content.as_deref(),
        Some("recovered")
    );
}

/// Per-fallback model override reaches the wire: the fallback request must
/// carry the override model, not the primary's.
#[tokio::test]
async fn model_override_reaches_fallback_request() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(503)
        .with_body("down")
        .expect(1)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .match_body(mockito::Matcher::Regex(r#""model":"cheaper-model""#.into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok_body("downgraded"))
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(
            openai_provider(fallback_srv.url()),
            Some("cheaper-model".to_string()),
        )],
    );

    let resp = chain
        .chat("expensive-model", &messages(), None)
        .await
        .unwrap();
    assert_eq!(
        resp.choices[0].message.content.as_deref(),
        Some("downgraded")
    );
}

/// Chain exhaustion surfaces the LAST fallback's error, not the primary's.
#[tokio::test]
async fn chain_exhaustion_returns_last_error() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fb1_srv = mockito::Server::new_async().await;
    let mut fb2_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body("limited")
        .expect(1)
        .create_async()
        .await;
    fb1_srv
        .mock("POST", "/chat/completions")
        .with_status(500)
        .with_body("fb1 down")
        .expect(1)
        .create_async()
        .await;
    fb2_srv
        .mock("POST", "/chat/completions")
        .with_status(502)
        .with_body("fb2 down")
        .expect(1)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![
            (openai_provider(fb1_srv.url()), None),
            (openai_provider(fb2_srv.url()), None),
        ],
    );

    let err = chain.chat("m", &messages(), None).await.unwrap_err();
    match err {
        Error::Http { status, body } => {
            assert_eq!(status, 502);
            assert!(body.contains("fb2 down"));
        }
        other => panic!("expected Error::Http, got: {other:?}"),
    }
}

/// Capabilities always describe the primary model: fallbacks are a
/// last-resort degradation, never the planning target.
#[tokio::test]
async fn capabilities_delegate_to_primary() {
    let primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_body(openai_ok_body("x"))
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(anthropic_provider(fallback_srv.url()), None)],
    );

    let caps = chain.capabilities("gpt-4o");
    assert_eq!(caps.max_input_tokens, 128_000);
    assert_eq!(caps.max_output_tokens, 16_384);
}

// ---------------------------------------------------------------------------
// Soak: sustained failure must degrade gracefully and deterministically
// ---------------------------------------------------------------------------

/// 200 sequential iterations with a permanently rate-limited primary: every
/// single request must recover via the fallback, with exactly one fallback
/// hit per iteration and zero unhandled errors.
#[tokio::test]
async fn soak_transient_primary_failure_always_recovers() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body("permanently limited")
        .expect(200)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok_body("soak-ok"))
        .expect(200)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    for iteration in 0..200 {
        let resp = chain
            .chat("m", &messages(), None)
            .await
            .unwrap_or_else(|e| panic!("iteration {iteration} failed: {e}"));
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("soak-ok"),
            "iteration {iteration} returned wrong payload"
        );
    }
}

/// 200 sequential iterations with a healthy primary: the fallback must
/// receive zero traffic — silent downgrades are a reliability bug.
#[tokio::test]
async fn soak_stable_primary_never_touches_fallback() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok_body("primary-ok"))
        .expect(200)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_body(openai_ok_body("never"))
        .expect(0)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    for iteration in 0..200 {
        let resp = chain
            .chat("m", &messages(), None)
            .await
            .unwrap_or_else(|e| panic!("iteration {iteration} failed: {e}"));
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("primary-ok"),
            "iteration {iteration} returned wrong payload"
        );
    }
}

/// 50 sequential streaming iterations against a dead primary: every stream
/// must recover via the fallback and drain to the full payload.
#[tokio::test]
async fn soak_streaming_fallback_stays_stable() {
    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(503)
        .with_body("outage")
        .expect(50)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(openai_sse_body("stream-soak"))
        .expect(50)
        .create_async()
        .await;

    let chain = FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    );

    for iteration in 0..50 {
        let stream = chain
            .chat_streaming("m", &messages(), None)
            .await
            .unwrap_or_else(|e| panic!("stream iteration {iteration} failed: {e}"));
        let text = drain_stream_text(stream).await;
        assert_eq!(text, "stream-soak", "iteration {iteration} lost payload");
    }
}

/// 64 concurrent requests against a rate-limited primary: the chain must be
/// safe under contention and every request must recover via the fallback.
#[tokio::test]
async fn soak_concurrent_burst_recovers_under_contention() {
    const TASKS: usize = 64;

    let mut primary_srv = mockito::Server::new_async().await;
    let mut fallback_srv = mockito::Server::new_async().await;

    primary_srv
        .mock("POST", "/chat/completions")
        .with_status(429)
        .with_body("limited")
        .expect(TASKS)
        .create_async()
        .await;
    fallback_srv
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok_body("burst-ok"))
        .expect(TASKS)
        .create_async()
        .await;

    let chain = Arc::new(FallbackChainProvider::new(
        openai_provider(primary_srv.url()),
        vec![(openai_provider(fallback_srv.url()), None)],
    ));

    let mut handles = Vec::with_capacity(TASKS);
    for _ in 0..TASKS {
        let chain = Arc::clone(&chain);
        handles.push(tokio::spawn(async move {
            chain.chat("m", &messages(), None).await
        }));
    }

    for (index, handle) in handles.into_iter().enumerate() {
        let resp = handle
            .await
            .unwrap_or_else(|e| panic!("task {index} panicked: {e}"))
            .unwrap_or_else(|e| panic!("task {index} failed: {e}"));
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("burst-ok"),
            "task {index} returned wrong payload"
        );
    }
}
