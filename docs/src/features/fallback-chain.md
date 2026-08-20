# Fallback Provider Chain (F3)

Automatic failover to backup LLM providers on transient errors.

## How It Works

`FallbackChainProvider` wraps a primary provider plus an ordered fallback list. On each request:

1. Try the current provider
2. If the error is **transient** (network failure, 429 rate limit, 5xx, interrupted stream) → advance to the next provider
3. Non-transient errors (auth failure, bad request) propagate immediately — no pointless retries

## Config

Default **OFF** (empty fallback list = primary only). Enable explicitly:

```toml
[fallback]
enabled = true

[[fallback.providers]]
provider = "openrouter"
model = "anthropic/claude-sonnet-4"

[[fallback.providers]]
provider = "gemini"
model = "gemini-2.5-pro"
```

## Implementation

- `kerux-core/src/client/fallback.rs` — `FallbackChainProvider` + `FallbackEntry` + `is_transient()` detection
- Wired in `kerux-cli` via `wrap_with_fallbacks()` around the runtime client
