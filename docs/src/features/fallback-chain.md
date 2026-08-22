# Fallback Provider Chain

Automatic failover to backup LLM providers on transient errors.

## How It Works

`FallbackChainProvider` wraps the primary provider plus an ordered fallback list. On each request:

1. Try the current provider
2. If the error is **fallback-worthy** (network failure, interrupted stream, 429 rate limit, 5xx) → advance to the next provider
3. Deterministic failures (401 auth, 400 bad request, context overflow) propagate immediately — no pointless retries

## Config

Default **OFF** (empty fallback list = primary only; the client stays locked to the single configured provider unless you opt in). Enable via `[[client.fallback]]` array-of-tables entries:

```toml
[client]
provider = "openai"

[[client.fallback]]
provider = "openrouter"
model = "anthropic/claude-sonnet-4"

[[client.fallback]]
provider = "gemini"
model = "gemini-2.5-pro"
```

Each entry supports `provider` (required), optional `base_url`, `api_key`, `model` (defaults to the primary model), and `timeout_secs`.

## Implementation

- `kerux-core/src/client/fallback.rs` — `FallbackChainProvider` + `FallbackEntry` + `is_fallback_worthy()` detection
- Wired in `kerux-cli` via `wrap_with_fallbacks()` around the runtime client
