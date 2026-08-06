# OAuth and provider authentication design

## Goal

Add provider authentication that can eventually support official browser login flows while keeping the current OpenAI-compatible API-key path intact.

This is a design checkpoint, not a runtime behavior change.

## Provider reality check

- **OpenAI**: public OpenAI-compatible API access continues to support API keys. OpenAI also documents ChatGPT/Codex auth for Codex clients, including browser login, device/headless login, access-token injection, and auth-cache reuse. Hermes should model this as a separate OpenAI/Codex account-auth capability, not silently treat Codex tokens as generic OpenAI API keys.
- **Google**: Gemini supports API keys and OAuth/Application Default Credentials. Direct desktop OAuth requires a Google OAuth client ID; ADC via `gcloud auth application-default login` keeps token creation, refresh, and storage outside Hermes.
- **GitHub Copilot**: official Copilot CLI authentication supports OAuth device flow, supported GitHub token types (`COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`), OS keychain storage, and GitHub CLI fallback. Hermes should reference external tokens first and only run Copilot login after provider-specific client behavior is defined.
- **Anthropic**: Claude access is not one single OAuth path. Documented routes include Claude.ai / Claude Code account login, Anthropic Console API keys, Team/Enterprise accounts, and cloud-provider routes such as Google Vertex AI, Amazon Bedrock, and Microsoft Foundry. Hermes should keep these as separate capabilities because Vertex/Bedrock/Foundry require provider-specific request/auth behavior, not just a bearer token swap.
- **OpenCode comparison**: OpenCode stores provider credentials outside project config and exposes `/connect` flows. Hermes should copy the credential separation pattern, not vendor-private auth internals.

## Recommended architecture

### 1. Keep project config non-secret

`hermes.toml` should continue to describe provider selection, base URLs, and model defaults. It should not become the default storage location for OAuth access tokens or refresh tokens.

Recommended future config shape:

```toml
[client]
provider = "openai-compatible"
base_url = "https://api.openai.com/v1"
auth_ref = "openai-default"
```

`auth_ref` points to an entry in local credential storage.

### 2. Store credential metadata locally; store secrets safely

Recommended metadata path:

- Windows: `%APPDATA%/hermes/auth.json`
- macOS: `~/Library/Application Support/hermes/auth.json`
- Linux: `~/.local/share/hermes/auth.json` or config-dir equivalent from existing platform helpers

Rules:

- Never write tokens to repo-local files.
- Never log token values.
- Bind credentials to the endpoint stored in the auth profile, and reject repo-local base URL overrides when an `auth_ref` is active.
- Require explicit base URLs for non-OpenAI profiles until provider-specific clients own their official endpoints.
- Prefer OS credential storage for long-lived secrets and refresh tokens.
- Recommended implementation: use platform credential storage (Windows Credential Manager, macOS Keychain, Linux Secret Service/libsecret) behind a small Hermes abstraction before persisting OAuth refresh tokens. Until that exists, keep tokens in environment variables or provider-managed stores such as Google ADC / GitHub CLI or Copilot CLI keychain.
- If OS credential storage is not implemented yet, keep long-lived secrets in environment variables or explicit config only; do not silently migrate them into plaintext JSON.
- If a plaintext fallback is ever added, it must be opt-in, clearly warned, and protected by best-effort owner-only file permissions.
- Store non-secret provider id, auth type, created/updated timestamps, expiry, and refresh metadata in `auth.json`.

Sketch:

```json
{
  "version": 1,
  "profiles": {
    "openai-default": {
      "provider": "openai",
      "method": "api_key",
      "base_url": "https://api.openai.com/v1",
      "secret_ref": "env:OPENAI_API_KEY"
    },
    "google-default": {
      "provider": "google-gemini",
      "method": "oauth_pkce",
      "scopes": ["provider-documented scopes for this flow"],
      "expires_at": "2026-01-01T00:00:00Z"
    }
  }
}
```

### 3. Add an auth provider boundary

Introduce a small internal provider-auth abstraction before adding provider-specific flows.

```rust
trait AuthProvider {
    fn id(&self) -> &'static str;
    fn supported_methods(&self) -> &'static [AuthMethod];
    async fn resolve_headers(&self, profile: &AuthProfile) -> Result<HeaderMap>;
}
```

Initial implementations should be minimal:

1. `ApiKeyAuthProvider` for the current OpenAI-compatible behavior.
2. `BearerTokenAuthProvider` for official OAuth/ADC access tokens where the provider accepts bearer tokens.

Provider-specific request formats should stay separate from auth. Hermes currently has an OpenAI-compatible client; OAuth should not imply that every provider can use `/v1/chat/completions`.

### 4. CLI/TUI flows

Future commands:

- `hermes auth login <provider>`
- `hermes auth set-api-key <provider>`
- `hermes auth set-bearer-token <provider> --env <ENV_VAR> --base-url <URL>`
- `hermes auth providers`
- `hermes auth list`
- `hermes auth logout <auth-ref>`
- TUI command/modal equivalent after CLI flow is stable

Login flow order:

1. Prefer API key where it is the official provider API path.
2. Prefer provider-managed credentials first: Google ADC, GitHub/Copilot CLI keychain, Claude Code setup wizards, AWS/GCP/Foundry credential chains.
3. Offer OAuth only for providers with documented third-party, device-code, or ADC flows.
4. Use loopback PKCE for native desktop OAuth where supported.
5. Support no-browser mode only through provider-documented flows such as device-code auth or external tools like `gcloud auth application-default login --no-browser`; do not invent copy/paste auth-code handling.

### 5. OAuth implementation constraints

Do not add OAuth until these are decided:

- Token storage format and permission model.
- Provider allowlist and scopes.
- Refresh behavior and expiry handling.
- How non-OpenAI-compatible providers map into `OpenAIClient` or a new client abstraction.
- How product-specific account auth maps to runtime endpoints, especially OpenAI Codex/ChatGPT auth and Anthropic Claude account auth.
- Whether adding OAuth crates is acceptable, or whether to implement PKCE/loopback using existing dependencies.
- Whether the project will use OS credential storage crates or keep OAuth behind external helper tools until secure storage exists.

Security requirements:

- Use PKCE for public/native clients.
- Bind redirect to loopback only (`127.0.0.1`), random port.
- Validate `state` and provider issuer/token endpoint.
- Loopback callbacks must accept only authorization codes plus validated `state`; never accept access tokens from query strings.
- Redact auth headers in logs.

## Phased implementation plan

### Phase 1: auth profiles, no OAuth

- Add local auth metadata store module.
- Add `hermes auth set-api-key <provider>` to create a profile that references an environment variable or explicitly configured key source; do not silently persist the secret itself.
- Move current API-key resolution behind auth profile lookup while preserving env/config behavior and precedence.
- Add `hermes auth list` and `hermes auth logout`.
- Tests: redacted list output, env precedence, missing-secret error, permission best-effort for metadata file.

### Phase 2: Google OAuth / ADC-compatible bearer auth

- Add Google as the first official OAuth-capable provider.
- Support existing ADC token discovery or explicit token helper before implementing full browser flow.
- Use provider-documented scopes per Gemini API vs Vertex AI flow; do not hardcode a single scope globally.
- Tests: expired token rejection/refresh boundary with mocked token provider.

Implemented Phase 2a:

- `hermes auth set-bearer-token <provider> --env <ENV_VAR> --base-url <URL>` stores metadata for externally managed OAuth/ADC bearer tokens.
- Hermes still does not run browser OAuth or refresh tokens itself.
- Bearer credentials use the same endpoint binding protections as API-key profiles.

Implemented Phase 2b:

- `hermes auth providers` reports provider aliases, documented auth methods, Hermes-supported environment sources, and implementation notes for Google, GitHub Copilot, OpenAI, and Anthropic.
- OpenAI Codex/ChatGPT auth and Anthropic Claude account/cloud-provider auth are documented as distinct capabilities instead of being collapsed into generic API-key or bearer-token auth.
- `hermes auth login <provider>` prints provider-specific external setup guidance and intentionally fails without creating credentials until secure token storage and provider-specific runtime clients are available.

### Phase 3: browser PKCE flow

- Add loopback OAuth helper.
- Add no-browser flow only for providers with a documented device-code or external-tool path.
- Tests: state validation, callback parsing, token exchange mock server, cleanup of local listener.

Implemented Phase 3a:

- Added provider-neutral PKCE/state helpers.
- Authorization URLs require `http://127.0.0.1:<port>/...` loopback redirects.
- Callback parsing accepts only authorization codes with matching `state` and rejects access tokens in query strings or fragments.
- Added a loopback callback receiver that binds only to `127.0.0.1` on a random local port and accepts one GET callback.
- Added provider-neutral authorization-code token exchange helper for PKCE flows. Token endpoints must use HTTPS; tests use loopback HTTP only through private test plumbing.
- Hermes still does not launch browsers or refresh/store OAuth tokens itself.

### Phase 4: provider-specific clients

- Add provider client abstraction only when the first non-OpenAI-compatible provider needs it.
- Keep OpenAI-compatible behavior unchanged.

## Non-goals for the first OAuth PR

- Reverse-engineered ChatGPT/Codex login beyond documented OpenAI flows.
- Reusing Claude Code private credential formats without documented support.
- Storing tokens in repo-local `hermes.toml`.
- Supporting every provider in one change.
