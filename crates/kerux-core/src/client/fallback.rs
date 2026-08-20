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
            Error::Agent(msg) => {
                let rest = msg.strip_prefix("HTTP ");
                matches!(rest, Some(code) if code.starts_with("429") || code.starts_with('5'))
            }
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

    struct ScriptedProvider {
        /// Results handed out in order; the last one repeats forever.
        /// Errors are stored as raw Agent-error strings so the exact
        /// "HTTP ..." prefix survives (no re-wrapping via Display).
        results: Vec<std::result::Result<ChatResponse, String>>,
        calls: Arc<AtomicUsize>,
    }

    impl ScriptedProvider {
        fn new(
            results: Vec<std::result::Result<ChatResponse, String>>,
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
                Err(msg) => Err(Error::Agent(msg.clone())),
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
        assert!(FallbackChainProvider::is_fallback_worthy(&Error::Agent(
            "HTTP 429: rate limited".into()
        )));
        assert!(FallbackChainProvider::is_fallback_worthy(&Error::Agent(
            "HTTP 503: upstream down".into()
        )));
        assert!(!FallbackChainProvider::is_fallback_worthy(&Error::Agent(
            "HTTP 401: unauthorized".into()
        )));
        assert!(!FallbackChainProvider::is_fallback_worthy(&Error::Agent(
            "HTTP 400: bad request".into()
        )));
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
            "HTTP 429: slow down".to_string(),
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
            "HTTP 401: bad key".to_string(),
        )]);
        let (fallback, fallback_calls) = ScriptedProvider::new(vec![Ok(ok_response("fallback"))]);
        let chain = FallbackChainProvider::new(primary, vec![(fallback, None)]);

        let err = chain.chat("m", &[], None).await.unwrap_err();
        assert!(err.to_string().contains("401"));
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn chain_exhaustion_returns_last_error() {
        let (primary, _) =
            ScriptedProvider::new(vec![Err::<ChatResponse, _>("HTTP 503: down".to_string())]);
        let (fb1, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(
            "HTTP 500: also down".to_string(),
        )]);
        let (fb2, _) =
            ScriptedProvider::new(vec![Err::<ChatResponse, _>("HTTP 502: nope".to_string())]);
        let chain = FallbackChainProvider::new(primary, vec![(fb1, None), (fb2, None)]);

        let err = chain.chat("m", &[], None).await.unwrap_err();
        assert!(err.to_string().contains("502"));
    }

    #[tokio::test]
    async fn second_fallback_wins_when_first_fails() {
        let (primary, _) = ScriptedProvider::new(vec![Err::<ChatResponse, _>(
            "HTTP 429: limited".to_string(),
        )]);
        let (fb1, _) =
            ScriptedProvider::new(vec![Err::<ChatResponse, _>("HTTP 500: down".to_string())]);
        let (fb2, fb2_calls) = ScriptedProvider::new(vec![Ok(ok_response("second"))]);
        let chain = FallbackChainProvider::new(primary, vec![(fb1, None), (fb2, None)]);

        let resp = chain.chat("m", &[], None).await.unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("second"));
        assert_eq!(fb2_calls.load(Ordering::SeqCst), 1);
    }
}
