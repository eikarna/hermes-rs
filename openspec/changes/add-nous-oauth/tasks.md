# Tasks: Native Nous Portal OAuth (device-code) authentication

## 1. Auth core (hermes-core/src/auth.rs)

- [x] Add `AuthMethod::Oauth` and `OAuthStoredTokens` struct
- [x] Add device-code helpers `request_nous_device_code`, `poll_nous_device_token`
- [x] Add `refresh_nous_access_token` (refresh grant)
- [x] Add constants `NOUS_PORTAL_URL`, `NOUS_INFERENCE_URL`, `NOUS_CLIENT_ID`, `NOUS_SCOPE`
- [x] Add `AuthStore::resolve_oauth_token` (async, refresh + persist on expiry, 120s skew)
- [x] Add `jwt_expiry` (manual base64url decode, no new dependency)
- [x] Add `oauth_profile_from_token_response` (builds `AuthProfile` from token response)
- [x] Extend `AuthProfile::validate` to accept `Oauth` without `secret_ref`
- [x] Add `oauth: None` to existing `AuthProfile` literals (upsert helpers, tests)

## 2. Provider + config (hermes-core)

- [x] Add `ProviderKind::Nous` (reuses `OpenAIClient`, default inference base_url)
- [x] Add `Nous` arms to all `provider.rs` match expressions
- [x] Add `Nous` arms to `config.rs` resolver match expressions (base_url/api_key/timeout/endpoint)

## 3. CLI wiring (hermes-cli/src/main.rs)

- [x] Make `runtime_client` async; add `runtime_client_with_store` for OAuth (resolves token, builds client with Bearer access token)
- [x] Add `login_nous()` device-code flow invoked by `auth login nous`
- [x] Add `Nous Portal` to `AUTH_PROVIDERS` (slug `nous`)
- [x] Add `.await` to all `runtime_client(...)` call sites

## 4. Docs

- [x] Document Nous OAuth in `hermes.example.toml`
- [x] Add "Nous Portal (OAuth device-code login)" section to `README.md`

## 5. Tests (hermes-core/src/auth.rs)

- [x] `jwt_expiry_decodes_exp_claim`
- [x] Rewrite device-code / poll / refresh / refresh+persist tests to use `mockito::Server` (mockito 1.7 API) instead of `mockito::mock(...)`
- [x] Run `cargo test -p hermes-core auth::` and confirm green

## 6. Verification

- [x] `cargo fmt --all`
- [x] `cargo check --workspace` (clean)
- [x] `cargo test --workspace` (hermes-core + hermes-cli green; pre-existing `mcp::stdio_client_connects_lists_and_calls_tool` fails only because `python` is absent from PATH — unrelated to this change)
- [x] `cargo clippy --workspace --all-targets --all-features -- -D warnings` (clean)
- [x] Release build: `cargo build --release` succeeds
- [x] Smoke: `./target/release/hermes auth providers` lists `nous`
- [ ] Smoke: `./target/release/hermes auth login nous` prints verification URL (interactive browser approval not automated)
