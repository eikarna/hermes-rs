# Fallback Provider Chain

Automatic failover to backup LLM providers on transient errors.

## How It Works

`FallbackChainProvider` wraps the primary provider plus an ordered fallback list. On each request:

1. Try the current provider
2. If the error is **fallback-worthy** (network failure, interrupted stream, 429 rate limit, 5xx) → advance to the next provider
3. Deterministic failures (401 auth, 400 bad request, context overflow) propagate immediately — no pointless retries

The classifier (`is_fallback_worthy`) branches on **typed** error variants only — it never sniffs free-form error text for "429-ish" strings. A provider that reports failures as unstructured prose is treated as a deterministic failure on purpose: guessing from prose risks silently downgrading you to a worse model. All built-in adapters (OpenAI-compatible, Anthropic, Gemini) emit typed `Error::Http { status, body }` on non-success responses, so 429/5xx classification is reliable across every provider.

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

## Operational Guidelines

- **Order matters.** Entries are tried strictly in declaration order; the first success wins. Put your cheapest/most-reliable backup first.
- **Model override per entry.** `model` pins that fallback to a specific model regardless of the primary's `[agent] model`. Omit it to reuse the primary model name on the fallback provider (only sensible for providers that share model naming, e.g. OpenRouter passthrough).
- **Streaming falls through too.** `chat_streaming` uses the same classifier; a stream that dies mid-SSE (`IncompleteSseMessage`) advances to the next provider.
- **Capabilities describe the primary.** Planning (context window, edit format, tool support) always uses the primary model's capability row — fallbacks are a last-resort degradation, not the planning target.
- **Chain exhaustion returns the last error.** If every provider fails, you get the final fallback's error, not the primary's.
- **Watch the logs.** Every failover emits a `warn!` with the fallback index, target model, and the triggering error; startup logs `Fallback provider chain enabled` with the entry count.
- **Auth is per entry.** A fallback with a bad key fails its own attempt and the chain moves on; a 401 on the *primary* does not trigger failover (deterministic).

## Verification

The chain is covered by unit tests (`client/fallback.rs`: classification boundaries, fallthrough, no-fallback on 401, exhaustion) and an integration/soak suite (`kerux-core/tests/fallback_chain.rs`) driving `FallbackChainProvider` against real mockito HTTP servers: cross-provider fallthrough (OpenAI/Anthropic/Gemini) on 429/5xx, streaming fallthrough with SSE drain, deterministic-401 no-fallback, model-override wire check, chain exhaustion, network-error recovery, plus soak runs (200 sequential iterations under a permanent rate limit, 200 stable-primary iterations, 50 streaming iterations, 64-task concurrent burst).

## Implementation

- `kerux-core/src/client/fallback.rs` — `FallbackChainProvider` + `FallbackEntry` + `is_fallback_worthy()` detection
- Wired in `kerux-cli` via `wrap_with_fallbacks()` around the runtime client
