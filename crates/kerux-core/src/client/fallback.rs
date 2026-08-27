//! Fallback provider chain (F3).
//!
//! Wraps a primary [`LLMProvider`] plus an ordered list of fallback
//! providers. When the primary fails with a *fallback-worthy* error
//! (network failure, HTTP 429 rate limit, or HTTP 5xx), the request is
//! retried against each fallback in order. Any other primary error
//! (auth failure, bad request, context overflow) is returned immediately:
//! retrying the same request elsewhere would fail the same way and could
//! silently downgrade the user to a worse model without them noticing.
//!
//! The chain is opt-in: an empty fallback list means the wrapper is never
//! constructed (see the CLI's `runtime_client`).

use std::sync::Arc;

use async_trait::async_trait;
use tracing::warn;

use crate::client::provider::{LLMProvider, ProviderCapabilities};
use crate::client::{ChatResponse, ChatStreamResponse, Message, ToolSchema};
use crate::error::{Error, Result};

/// One fallback entry: the provider client plus an optional model
/// override (falls back to the requested model when `None`).
pub type FallbackEntry = (Arc<dyn LLMProvider>, Option<String>);

/// Primary provider plus ordered fallbacks.
pub struct FallbackChainProvider {
    primary: Arc<dyn LLMProvider>,
    fallbacks: Vec<FallbackEntry>,
}

impl FallbackChainProvider {
    pub fn new(primary: Arc<dyn LLMProvider>, fallbacks: Vec<FallbackEntry>) -> Self {
        Self { primary, fallbacks }
    }

    /// Whether a primary-provider error should trigger the fallback chain.
    ///
    /// Only transient/infrastructure failures qualify: network errors,
    /// incomplete streams, HTTP 429 (rate limit) and HTTP 5xx (upstream
    /// outage). Deterministic failures (401, 400, context overflow) are
    /// returned as-is.
    pub fn is_fallback_worthy(error: &Error) -> bool {
        match error {
            Error::Network(_) | Error::IncompleteSseMessage => true,
            // Typed status check — never parse Display strings. A provider
            // that reports failures as unstructured text is treated as a
            // deterministic failure on purpose: guessing "429-ish" from
            // free-form prose risks silently downgrading the user to a
            // worse model.
            Error::Http { status, .. } => *status == 429 || (500..600).contains(status),
            _ => false,
        }
    }
}

#[async_trait]
impl LLMProvider for FallbackChainProvider {
    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        let primary_err = match self.primary.chat(model, messages, tools).await {
            Ok(value) => return Ok(value),
            Err(e) if Self::is_fallback_worthy(&e) => e,
            Err(e) => return Err(e),
        };

        let mut last_err = primary_err;
        for (index, (provider, model_override)) in self.fallbacks.iter().enumerate() {
            let fallback_model = model_override.as_deref().unwrap_or(model);
            warn!(
                fallback = index + 1,
                model = %fallback_model,
                error = %last_err,
                "Primary provider failed; trying fallback for chat"
            );
            match provider.chat(fallback_model, messages, tools).await {
                Ok(value) => return Ok(value),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    async fn chat_streaming(
        &self,
        model: &str,
        messages: &[Message],
        tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        let primary_err = match self.primary.chat_streaming(model, messages, tools).await {
            Ok(value) => return Ok(value),
            Err(e) if Self::is_fallback_worthy(&e) => e,
            Err(e) => return Err(e),
        };

        let mut last_err = primary_err;
        for (index, (provider, model_override)) in self.fallbacks.iter().enumerate() {
            let fallback_model = model_override.as_deref().unwrap_or(model);
            warn!(
                fallback = index + 1,
                model = %fallback_model,
                error = %last_err,
                "Primary provider failed; trying fallback for streaming chat"
            );
            match provider
                .chat_streaming(fallback_model, messages, tools)
                .await
            {
                Ok(value) => return Ok(value),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        // Capabilities describe the primary model: fallbacks are a
        // last-resort degradation, not the planning target.
        self.primary.capabilities(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Choice, MessageDelta, Role, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test-local scripted failure. Converted into a real [`Error`] at
    /// call time (`Error` itself is not `Clone`). Mirrors the typed HTTP
    /// failures the live client now emits.
    #[derive(Clone)]
    enum ScriptedFailure {
        Http { status: u16, body: String },
    }

    impl ScriptedFailure {
        fn http(status: u16, body: &str) -> Self {
            Self::Http {
                status,
                body: body.to_string(),
            }
        }

        fn into_error(self) -> Error {
            let Self::Http { status, body } = self;
            Error::Http { status, body }
        }
    }

    struct ScriptedProvider {
        /// Results handed out in order; the last one repeats forever.
        results: Vec<std::result::Result<ChatResponse, ScriptedFailure>>,
        calls: Arc<AtomicUsize>,
    }

    impl ScriptedProvider {
        fn new(
            results: Vec<std::result::Result<ChatResponse, ScriptedFailure>>,
        ) -> (Arc<Self>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Arc::new(Self {
                    results,
                    calls: calls.clone(),
                }),
                calls,
            )
        }
    }

    fn ok_response(text: &str) -> ChatResponse {
        ChatResponse {
            id: "test".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "test".to_string(),
            choices: vec![Choice {
                index: 0,
                message: MessageDelta {
                    role: Some(Role::Assistant),
                    content: Some(text.to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cached_prompt_tokens: 0,
            },
        }
    }

    #[async_trait]
    impl LLMProvider for ScriptedProvider {
        async fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[ToolSchema]>,
        ) -> Result<ChatResponse> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let idx = n.min(self.results.len() - 1);
            match &self.results[idx] {
                Ok(r) => Ok(r.clone()),
                Err(failure) => Err(failure.clone().into_error()),
            }
        }

        async fn chat_streaming(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[ToolSchema]>,
        ) -> Result<ChatStreamResponse> {
            Err(Error::Agent("streaming not scripted".to_string()))
        }

        fn capabilities(&self, _model: &str) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
    }

    #[test]
    fn fallback_worthy_classification() {
        assert!(FallbackChainProvider::is_fallback_worthy(&Error::Http {
            status: 429,
            body: "rate limited".into()
        }));
        assert!(FallbackChainProvider::is_fallback_worthy(&Error::Http {
            status: 503,
            body: "upstream down".into()
        }));
        // Boundary: 500 and 599 both qualify.
        assert!(FallbackChainProvider::is_fallback_worthy(&Error::Http {
            status: 500,
            body: String::new()
        }));
        assert!(FallbackChainProvider::is_fallback_worthy(&Error::Http {
            status: 599,
            body: String::new()
        }));
        assert!(!FallbackChainProvider::is_fallback_worthy(&Error::Http {
            status: 401,
            body: "unauthorized".into()
        }));
        assert!(!FallbackChainProvider::is_fallback_worthy(&Error::Http {
            status: 400,
            body: "bad request".into()
        }));
        // Free-form agent text must NOT be sniffed for "HTTP"-like prefixes.
        let legacy_style = Error::Agent("HTTP 429: rate limited".into());
        assert!(!FallbackChainProvider::is_fallback_worthy(&legacy_style));
        assert!(!FallbackChainProvider::is_fallback_worthy(
            &Error::MissingApiKey
        ));
        assert!(FallbackChainProvider::is_fallback_worthy(
            &Error::IncompleteSseMessage
        ));
    }

    #[tokio::test]
    async fn primary_success_skips_fallbacks() {
        let (primary, primary_calls) = ScriptedProvider::new(vec![Ok(ok_response("primary"))]);
        let (fallback, fallback_calls) = ScriptedProvider::new(vec![Ok(ok_response("fallback"))]);
        let chain = FallbackChainProvider::new(primary, vec![(fallback, None)]);

        let resp = chain.chat("m", &[], None).await.unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("primary"));
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rate_limit_falls_through_to_fallback() {
        let (primary, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(
            ScriptedFailure::http(429, "slow down"),
        )]);
        let (fallback, fallback_calls) = ScriptedProvider::new(vec![Ok(ok_response("fallback"))]);
        let chain = FallbackChainProvider::new(primary, vec![(fallback, None)]);

        let resp = chain.chat("m", &[], None).await.unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("fallback"));
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deterministic_error_does_not_fall_back() {
        let (primary, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(
            ScriptedFailure::http(401, "bad key"),
        )]);
        let (fallback, fallback_calls) = ScriptedProvider::new(vec![Ok(ok_response("fallback"))]);
        let chain = FallbackChainProvider::new(primary, vec![(fallback, None)]);

        let err = chain.chat("m", &[], None).await.unwrap_err();
        assert!(err.to_string().contains("401"));
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn chain_exhaustion_returns_last_error() {
        let (primary, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(
            ScriptedFailure::http(503, "down"),
        )]);
        let (fb1, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(ScriptedFailure::http(
            500,
            "also down",
        ))]);
        let (fb2, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(ScriptedFailure::http(
            502, "nope",
        ))]);
        let chain = FallbackChainProvider::new(primary, vec![(fb1, None), (fb2, None)]);

        let err = chain.chat("m", &[], None).await.unwrap_err();
        assert!(err.to_string().contains("502"));
    }

    #[tokio::test]
    async fn second_fallback_wins_when_first_fails() {
        let (primary, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(
            ScriptedFailure::http(429, "limited"),
        )]);
        let (fb1, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(ScriptedFailure::http(
            500, "down",
        ))]);
        let (fb2, fb2_calls) = ScriptedProvider::new(vec![Ok(ok_response("second"))]);
        let chain = FallbackChainProvider::new(primary, vec![(fb1, None), (fb2, None)]);

        let resp = chain.chat("m", &[], None).await.unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("second"));
        assert_eq!(fb2_calls.load(Ordering::SeqCst), 1);
    }
}
