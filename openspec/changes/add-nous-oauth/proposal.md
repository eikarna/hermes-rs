# Proposal: Native Nous Portal OAuth (device-code) authentication

## Status

Proposed

## Context

`hermes-rs` currently supports four LLM providers (`openai`, `anthropic`, `ollama`,
`openrouter`, `gemini`) and an auth subsystem (`AuthMethod::{ApiKey, BearerToken}`)
that resolves credentials from environment-variable references. It cannot authenticate
against **Nous Portal** without a manual API/subscription-proxy token pasted into
`NOUS_API_KEY` and pointed at the OpenAI-compatible inference endpoint
`https://inference-api.nousresearch.com/v1`.

The upstream Python agent (`NousResearch/hermes-agent`) authenticates with Nous Portal
via an **OAuth device-code flow** (`hermes auth add nous --type oauth`). The returned
access token is already a short-lived invoke JWT (scope `inference:invoke`) valid against
the inference endpoint; a long-lived refresh token is persisted and used to mint new
access tokens. `hermes-rs` has no equivalent, so Python-Hermes subscription users cannot
use their existing Portal login with the Rust binary.

### Why now

The user wants the Rust version to consume the same Nous Portal OAuth login as the
Python agent, so a Portal subscription works end-to-end without copying bearer tokens
into environment variables.

## Goal

Add a first-class `nous` provider and an `OAuth` auth method that:

1. Runs the Nous Portal device-code flow (`hermes auth login nous`), printing a
   verification URL for the user to approve in a browser.
2. Persists the resulting access + refresh tokens in the existing auth store
   (`auth.json`) as an `OAuth` profile named `nous-default`.
3. Automatically refreshes the access token (via the refresh grant) before each
   inference request when it is within 120s of JWT expiry, persisting rotated tokens.
4. Binds the `nous` provider to the OpenAI-compatible inference endpoint so chat/stream
   requests carry `Authorization: Bearer <access_token>`.

## Approach

### Auth method (hermes-core/src/auth.rs)

- New `AuthMethod::Oauth` variant and `OAuthStoredTokens` struct
  (`access_token`, `refresh_token`, `client_id`, `portal_base_url`,
  `inference_base_url`, `token_type`, `expires_at`, `scope`). Stored directly in the
  auth store (the only on-disk secret), mirroring upstream behavior — `secret_ref` is
  unused for OAuth.
- Device-code helpers: `request_nous_device_code(portal_url, client_id, scope)`,
  `poll_nous_device_token(...)` (handles `authorization_pending`/`slow_down`),
  `refresh_nous_access_token(portal_url, client_id, refresh_token)`.
- Constants: `NOUS_PORTAL_URL = "https://portal.nousresearch.com"`,
  `NOUS_INFERENCE_URL = "https://inference-api.nousresearch.com/v1"`,
  `NOUS_CLIENT_ID = "hermes-cli"`, `NOUS_SCOPE = "inference:invoke"`.
- `AuthStore::resolve_oauth_token(&mut self, name)` (async): returns a live access
  token, refreshing + persisting when `jwt_expiry(access_token)` is within 120s of now.
- `jwt_expiry(token)` decodes the `exp` claim (manual base64url, no new dependency).
- `validate()` allows `Oauth` profiles without `secret_ref` but requires both tokens
  and both base URLs.

### Provider (hermes-core/src/client/provider.rs, config.rs)

- New `ProviderKind::Nous` → reuses `OpenAIClient` (OpenAI-compatible transport),
  default `base_url = NOUS_INFERENCE_URL`. Added to every `match` in `provider.rs` and
  the resolver `match` arms in `config.rs`.

### Wiring (hermes-cli/src/main.rs)

- `runtime_client` is now `async`; for an `OAuth` `auth_ref` it calls
  `runtime_client_with_store`, which resolves the live token and builds the client
  with `api_key = Some(access_token)` and `base_url = inference_base_url`.
- `auth login <provider>`: for `nous` runs `login_nous()` (device-code flow → persists
  `nous-default`); other providers keep the existing guidance-only behavior.
- `AUTH_PROVIDERS` gains a `Nous Portal` entry (slug `nous`).
- `hermes.example.toml` and `README.md` document the flow.

## Affected files

- `crates/hermes-core/src/auth.rs` (new `Oauth` method, device-code + refresh, tests)
- `crates/hermes-core/src/client/provider.rs` (`ProviderKind::Nous`)
- `crates/hermes-core/src/config.rs` (resolver match arms)
- `crates/hermes-core/src/client.rs` (`apply_auth_profile` Oauth branch)
- `crates/hermes-cli/src/main.rs` (async `runtime_client`, `login_nous`, provider list)
- `hermes.example.toml`, `README.md` (docs)
- `openspec/changes/add-nous-oauth/specs/...` (capability delta, below)

## Non-goals

- No browser/loopback (PKCE authorization-code) flow — Nous Portal uses device-code.
- No separate "mint invoke_jwt" call; the device-code/token access token already is
  the invoke JWT for scope `inference:invoke`.
- No `gcloud`/Google ADC or other provider login flows — only `nous` is implemented.
- Tokens are stored in plaintext `auth.json` (same trust boundary as upstream); no OS
  keychain integration in this change.

## Risks / open questions

- Exact Portal token-endpoint response shape (field names, `expires_in` presence) is
  derived from upstream `auth.py` inspection; if Portal changes it, the `serde` model
  may need adjustment. Covered by mockito tests against the documented shape.
- `client_id` is hardcoded to `hermes-cli` (matches upstream CLI). If Portal issues
  per-client IDs, this becomes configurable later.
