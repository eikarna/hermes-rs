use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};

use crate::error::{Error, Result};
use crate::platform::{kerux_data_dir, set_file_permissions, set_secure_permissions};

const AUTH_STORE_VERSION: u32 = 1;
const BASE64_URL_SAFE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

// ---------------------------------------------------------------------------
// Nous Portal OAuth (device-code flow)
// ---------------------------------------------------------------------------
/// Default Nous Portal base URL for token operations.
pub const NOUS_PORTAL_URL: &str = "https://portal.nousresearch.com";
/// Default Nous Portal inference API base URL (OpenAI-compatible).
pub const NOUS_INFERENCE_URL: &str = "https://inference-api.nousresearch.com/v1";
/// Public OAuth client id registered for this CLI with Nous Portal.
/// This protocol identifier intentionally retains the historical product name.
pub const NOUS_CLIENT_ID: &str = "hermes-cli";
/// Scope granting inference invoke access. The returned access token is itself
/// a short-lived invoke JWT usable directly as the inference bearer.
pub const NOUS_SCOPE: &str = "inference:invoke";
/// Refresh the access token this many seconds before it expires.
const NOUS_TOKEN_REFRESH_SKEW_SECONDS: u64 = 120;

/// Shared HTTP client for the synchronous-looking Portal OAuth calls. Created
/// once per process; avoids building a new connection pool on every login or
/// token refresh. `reqwest::Client` is cheap to clone (internal `Arc`), so we
/// return an owned copy of the singleton.
fn http_client() -> reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new).clone()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AuthStore {
    pub version: u32,
    pub profiles: BTreeMap<String, AuthProfile>,
}

impl Default for AuthStore {
    fn default() -> Self {
        Self {
            version: AUTH_STORE_VERSION,
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AuthProfile {
    pub provider: String,
    pub method: AuthMethod,
    pub base_url: Option<String>,
    pub secret_ref: String,
    pub disabled: bool,
    /// Persisted OAuth tokens for `AuthMethod::Oauth`. Stored directly in the
    /// auth store (the only credential on disk) rather than referenced via an
    /// environment variable, matching the upstream Nous Portal behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthStoredTokens>,
}

impl Default for AuthProfile {
    fn default() -> Self {
        Self {
            provider: String::new(),
            method: AuthMethod::ApiKey,
            base_url: None,
            secret_ref: String::new(),
            disabled: false,
            oauth: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    #[default]
    ApiKey,
    BearerToken,
    /// OAuth device-code flow (e.g. Nous Portal). Tokens are persisted in
    /// `AuthProfile::oauth`; `secret_ref` is unused.
    Oauth,
}

/// Persisted OAuth token state for a device-code profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OAuthStoredTokens {
    /// Short-lived access token (an invoke JWT for Nous Portal).
    pub access_token: String,
    /// Long-lived refresh token.
    pub refresh_token: String,
    pub client_id: String,
    /// Portal base URL used for token operations.
    pub portal_base_url: String,
    /// Inference API base URL the access token is valid against.
    pub inference_base_url: String,
    pub token_type: String,
    /// Unix epoch seconds when `access_token` expires. `0` when unknown.
    pub expires_at: u64,
    /// Scope granted (e.g. `inference:invoke`).
    pub scope: String,
}

impl Default for OAuthStoredTokens {
    fn default() -> Self {
        Self {
            access_token: String::new(),
            refresh_token: String::new(),
            client_id: NOUS_CLIENT_ID.to_string(),
            portal_base_url: NOUS_PORTAL_URL.to_string(),
            inference_base_url: NOUS_INFERENCE_URL.to_string(),
            token_type: "Bearer".to_string(),
            expires_at: 0,
            scope: NOUS_SCOPE.to_string(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PkceChallenge {
    pub code_verifier: String,
    pub code_challenge: String,
    pub code_challenge_method: &'static str,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: Option<u64>,
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
}

#[derive(Debug)]
pub struct LoopbackOAuthReceiver {
    listener: TcpListener,
    redirect_uri: String,
    callback_path: String,
}

impl LoopbackOAuthReceiver {
    pub async fn bind(callback_path: &str) -> Result<Self> {
        let callback_path = normalize_callback_path(callback_path)?;
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| Error::Config(format!("Failed to bind OAuth loopback: {}", error)))?;
        let port = listener
            .local_addr()
            .map_err(|error| {
                Error::Config(format!("Failed to read OAuth loopback port: {}", error))
            })?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{}{}", port, callback_path);

        Ok(Self {
            listener,
            redirect_uri,
            callback_path,
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub async fn wait_for_code(
        self,
        expected_state: &str,
        timeout_duration: Duration,
    ) -> Result<String> {
        let (mut stream, _) = timeout(timeout_duration, self.listener.accept())
            .await
            .map_err(|_| Error::Config("Timed out waiting for OAuth callback".to_string()))?
            .map_err(|error| {
                Error::Config(format!("Failed to accept OAuth callback: {}", error))
            })?;

        let mut buffer = [0_u8; 8192];
        let bytes_read = timeout(timeout_duration, stream.read(&mut buffer))
            .await
            .map_err(|_| Error::Config("Timed out reading OAuth callback".to_string()))?
            .map_err(|error| Error::Config(format!("Failed to read OAuth callback: {}", error)))?;
        let request = std::str::from_utf8(&buffer[..bytes_read])
            .map_err(|_| Error::Config("OAuth callback was not valid UTF-8".to_string()))?;

        let result = parse_http_callback_request(
            request,
            &self.redirect_uri,
            &self.callback_path,
            expected_state,
        );
        let _ = write_oauth_callback_response(&mut stream, result.is_ok()).await;
        result
    }
}

impl AuthStore {
    pub fn load_default() -> Result<Self> {
        Self::load_from(default_auth_store_path())
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(path).map_err(|error| {
            Error::Config(format!(
                "Failed to read auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        let store: Self = serde_json::from_str(&raw).map_err(|error| {
            Error::Config(format!(
                "Failed to parse auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        store.validate()?;
        Ok(store)
    }

    pub fn save_default(&self) -> Result<()> {
        self.save_to(default_auth_store_path())
    }

    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::Config(format!(
                    "Failed to create auth store directory '{}': {}",
                    parent.display(),
                    error
                ))
            })?;
            set_secure_permissions(parent).map_err(|error| {
                Error::Config(format!(
                    "Failed to secure auth store directory '{}': {}",
                    parent.display(),
                    error
                ))
            })?;
        }

        let raw = serde_json::to_string_pretty(self)?;
        fs::write(path, raw).map_err(|error| {
            Error::Config(format!(
                "Failed to write auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        set_file_permissions(path, 0o600).map_err(|error| {
            Error::Config(format!(
                "Failed to secure auth store '{}': {}",
                path.display(),
                error
            ))
        })?;
        Ok(())
    }

    pub fn upsert_api_key_env_profile(
        &mut self,
        name: impl Into<String>,
        provider: impl Into<String>,
        env_var: impl Into<String>,
        base_url: Option<String>,
    ) -> Result<()> {
        let name = validate_name("auth profile", name.into())?;
        let provider = validate_name("provider", provider.into())?;
        let env_var = validate_env_var(env_var.into())?;
        let base_url = validate_base_url(base_url)?;

        self.profiles.insert(
            name,
            AuthProfile {
                provider,
                method: AuthMethod::ApiKey,
                base_url,
                secret_ref: format!("env:{}", env_var),
                disabled: false,
                oauth: None,
            },
        );
        Ok(())
    }

    pub fn upsert_bearer_token_env_profile(
        &mut self,
        name: impl Into<String>,
        provider: impl Into<String>,
        env_var: impl Into<String>,
        base_url: Option<String>,
    ) -> Result<()> {
        let name = validate_name("auth profile", name.into())?;
        let provider = validate_name("provider", provider.into())?;
        let env_var = validate_env_var(env_var.into())?;
        let base_url = validate_base_url(base_url)?.ok_or_else(|| {
            Error::Config("Bearer token auth profiles require a base URL".to_string())
        })?;

        self.profiles.insert(
            name,
            AuthProfile {
                provider,
                method: AuthMethod::BearerToken,
                base_url: Some(base_url),
                secret_ref: format!("env:{}", env_var),
                disabled: false,
                oauth: None,
            },
        );
        Ok(())
    }

    pub fn remove_profile(&mut self, name: &str) -> bool {
        self.profiles.remove(name).is_some()
    }

    pub fn resolve_api_key(&self, name: &str) -> Result<String> {
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| Error::MissingConfig {
                key: format!("auth profile '{}'", name),
            })?;
        profile.resolve_api_key()
    }

    pub fn resolve_auth_token(&self, name: &str) -> Result<String> {
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| Error::MissingConfig {
                key: format!("auth profile '{}'", name),
            })?;
        profile.resolve_auth_token()
    }

    fn validate(&self) -> Result<()> {
        if self.version != AUTH_STORE_VERSION {
            return Err(Error::Config(format!(
                "Unsupported auth store version: {}",
                self.version
            )));
        }
        for (name, profile) in &self.profiles {
            validate_name("auth profile", name.clone())?;
            profile.validate()?;
        }
        Ok(())
    }
}

impl AuthProfile {
    pub fn resolved_env_var(&self) -> Option<&str> {
        self.secret_ref.strip_prefix("env:")
    }

    pub fn resolve_api_key(&self) -> Result<String> {
        if self.method != AuthMethod::ApiKey {
            return Err(Error::Config(format!(
                "Auth profile '{}' is not an API key profile",
                self.provider
            )));
        }
        self.resolve_auth_token()
    }

    pub fn resolve_auth_token(&self) -> Result<String> {
        self.validate()?;
        if self.disabled {
            return Err(Error::Config(format!(
                "Auth profile '{}' is disabled",
                self.provider
            )));
        }
        match self.method {
            AuthMethod::ApiKey | AuthMethod::BearerToken => {}
            AuthMethod::Oauth => {
                return Err(Error::Config(format!(
                    "Auth profile '{}' is an OAuth profile; use resolve_oauth_token instead",
                    self.provider
                )));
            }
        }

        let env_var = self.resolved_env_var().ok_or_else(|| {
            Error::Config(format!(
                "Auth profile '{}' uses an unsupported secret reference",
                self.provider
            ))
        })?;

        env::var(env_var)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::MissingConfig {
                key: format!("environment variable {} for auth profile", env_var),
            })
    }

    fn validate(&self) -> Result<()> {
        validate_name("provider", self.provider.clone())?;
        match self.method {
            AuthMethod::Oauth => {
                let tokens = self.oauth.as_ref().ok_or_else(|| {
                    Error::Config(format!(
                        "OAuth profile '{}' has no stored tokens",
                        self.provider
                    ))
                })?;
                if tokens.access_token.is_empty() || tokens.refresh_token.is_empty() {
                    return Err(Error::Config(format!(
                        "OAuth profile '{}' is missing access or refresh token",
                        self.provider
                    )));
                }
                if tokens.portal_base_url.is_empty() || tokens.inference_base_url.is_empty() {
                    return Err(Error::Config(format!(
                        "OAuth profile '{}' is missing a Portal or inference base URL",
                        self.provider
                    )));
                }
            }
            AuthMethod::ApiKey | AuthMethod::BearerToken => {
                if self.resolved_env_var().is_none() {
                    return Err(Error::Config(format!(
                        "Auth profile '{}' uses an unsupported secret reference",
                        self.provider
                    )));
                }
                if self.method == AuthMethod::BearerToken && self.base_url.is_none() {
                    return Err(Error::Config(format!(
                        "Bearer auth profile '{}' requires a base URL",
                        self.provider
                    )));
                }
            }
        }
        let _ = validate_base_url(self.base_url.clone())?;
        Ok(())
    }
}

pub fn default_auth_store_path() -> PathBuf {
    if let Ok(path) = env::var("KERUX_AUTH_STORE") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    kerux_data_dir().join("auth.json")
}

pub fn generate_oauth_state() -> Result<String> {
    random_base64_url(32)
}

pub fn generate_pkce_challenge() -> Result<PkceChallenge> {
    let code_verifier = random_base64_url(32)?;
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64_url_no_pad(&digest);

    Ok(PkceChallenge {
        code_verifier,
        code_challenge,
        code_challenge_method: "S256",
    })
}

pub fn build_oauth_authorization_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
    pkce: &PkceChallenge,
) -> Result<String> {
    validate_loopback_redirect_uri(redirect_uri)?;
    let mut url = reqwest::Url::parse(authorization_endpoint)
        .map_err(|error| Error::InvalidUrl(error.to_string()))?;
    if url.scheme() != "https" {
        return Err(Error::Config(
            "OAuth authorization endpoint must use https".to_string(),
        ));
    }
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.code_challenge)
        .append_pair("code_challenge_method", pkce.code_challenge_method);
    Ok(url.into())
}

pub fn parse_loopback_authorization_code(
    callback_url: &str,
    expected_state: &str,
) -> Result<String> {
    let url =
        reqwest::Url::parse(callback_url).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    validate_loopback_redirect_uri(callback_url)?;

    if url.fragment().is_some_and(fragment_contains_access_token) {
        return Err(Error::Config(
            "OAuth callback must not include access tokens".to_string(),
        ));
    }

    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "access_token" => {
                return Err(Error::Config(
                    "OAuth callback must not include access tokens".to_string(),
                ));
            }
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }

    let state = state.ok_or_else(|| Error::Config("OAuth callback missing state".to_string()))?;
    if state != expected_state {
        return Err(Error::Config("OAuth callback state mismatch".to_string()));
    }

    code.filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Config("OAuth callback missing authorization code".to_string()))
}

pub async fn exchange_oauth_authorization_code(
    token_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthTokenResponse> {
    let client = http_client();
    exchange_oauth_authorization_code_with_client(
        &client,
        token_endpoint,
        client_id,
        redirect_uri,
        code,
        code_verifier,
        false,
    )
    .await
}

async fn exchange_oauth_authorization_code_with_client(
    client: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    allow_loopback_http: bool,
) -> Result<OAuthTokenResponse> {
    validate_oauth_token_endpoint(token_endpoint, allow_loopback_http)?;
    validate_loopback_redirect_uri(redirect_uri)?;
    validate_oauth_form_value("client_id", client_id)?;
    validate_oauth_form_value("code", code)?;
    validate_oauth_form_value("code_verifier", code_verifier)?;

    let response = client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("code", code),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        return Err(Error::Config(format!(
            "OAuth token exchange failed with status {}",
            status
        )));
    }

    let token = response.json::<OAuthTokenResponse>().await?;
    if token.access_token.is_empty() {
        return Err(Error::Config(
            "OAuth token response missing access token".to_string(),
        ));
    }
    if !token.token_type.eq_ignore_ascii_case("bearer") {
        return Err(Error::Config(
            "OAuth token response must use Bearer token type".to_string(),
        ));
    }
    Ok(token)
}

// ---------------------------------------------------------------------------
// Nous Portal device-code flow
// ---------------------------------------------------------------------------

/// Result of requesting a device code from the Portal.
#[derive(Debug, Clone)]
pub struct NousDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Request a device code from the Nous Portal. The user opens
/// `verification_uri_complete` to approve the login.
pub async fn request_nous_device_code(
    portal_base_url: &str,
    client_id: &str,
    scope: &str,
) -> Result<NousDeviceCode> {
    let client = http_client();
    let response = client
        .post(format!(
            "{}/api/oauth/device/code",
            portal_base_url.trim_end_matches('/')
        ))
        .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Config(format!(
            "Nous Portal device code request failed ({}): {}",
            status, body
        )));
    }
    let data: serde_json::Value = response.json().await?;
    let device_code = data
        .get("device_code")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Config("Device code response missing device_code".to_string()))?
        .to_string();
    let user_code = data
        .get("user_code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let verification_uri_complete = data
        .get("verification_uri_complete")
        .and_then(Value::as_str)
        .or_else(|| data.get("verification_uri").and_then(Value::as_str))
        .ok_or_else(|| Error::Config("Device code response missing verification_uri".to_string()))?
        .to_string();
    let expires_in = data
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(600);
    let interval = data.get("interval").and_then(Value::as_u64).unwrap_or(5);
    Ok(NousDeviceCode {
        device_code,
        user_code,
        verification_uri_complete,
        expires_in,
        interval: interval.max(1),
    })
}

/// Poll the token endpoint until the user approves the device code or it
/// expires. Handles `authorization_pending` and `slow_down` per RFC 8628.
pub async fn poll_nous_device_token(
    portal_base_url: &str,
    client_id: &str,
    device_code: &str,
    expires_in: u64,
    interval: u64,
) -> Result<OAuthTokenResponse> {
    let client = http_client();
    let token_url = format!("{}/api/oauth/token", portal_base_url.trim_end_matches('/'));
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in);
    let mut current_interval = interval.max(1);

    loop {
        if std::time::Instant::now() >= deadline {
            return Err(Error::Config(
                "Timed out waiting for device authorization. Sign in at the Portal, then retry."
                    .to_string(),
            ));
        }

        let response = client
            .post(&token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", device_code),
            ])
            .send()
            .await?;

        if response.status().is_success() {
            let token: OAuthTokenResponse = response.json().await?;
            if !token.access_token.is_empty() {
                return Ok(token);
            }
            return Err(Error::Config(
                "Nous Portal token response missing access_token".to_string(),
            ));
        }

        let error_payload: serde_json::Value =
            response.json().await.unwrap_or(serde_json::json!({}));
        let error_code = error_payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match error_code {
            "authorization_pending" => {
                tokio::time::sleep(Duration::from_secs(current_interval)).await
            }
            "slow_down" => {
                current_interval = (current_interval + 1).min(30);
                tokio::time::sleep(Duration::from_secs(current_interval)).await
            }
            other => {
                let description = error_payload
                    .get("error_description")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown authentication error");
                return Err(Error::Config(format!(
                    "Nous Portal auth error: {}: {}",
                    other, description
                )));
            }
        }
    }
}

/// Exchange a refresh token for a new access token at the Portal token endpoint.
pub async fn refresh_nous_access_token(
    portal_base_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<OAuthTokenResponse> {
    let client = http_client();
    let response = client
        .post(format!(
            "{}/api/oauth/token",
            portal_base_url.trim_end_matches('/')
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Config(format!(
            "Nous Portal token refresh failed ({}): {}",
            status, body
        )));
    }
    let token: OAuthTokenResponse = response.json().await?;
    if token.access_token.is_empty() {
        return Err(Error::Config(
            "Nous Portal refresh response missing access_token".to_string(),
        ));
    }
    Ok(token)
}

/// Decode the `exp` claim (unix seconds) from a JWT without verifying the
/// signature. Returns `None` when the token is not a parsable JWT or lacks `exp`.
pub fn jwt_expiry(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    // base64url (no padding) decode using the project's own safe alphabet.
    let mut bits: u32 = 0;
    let mut bits_left: u32 = 0;
    let mut raw = Vec::new();
    for ch in payload.chars() {
        let value = BASE64_URL_SAFE.iter().position(|&b| b as char == ch)? as u32;
        bits = (bits << 6) | value;
        bits_left += 6;
        if bits_left >= 8 {
            bits_left -= 8;
            raw.push((bits >> bits_left) as u8);
        }
    }
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    value.get("exp").and_then(Value::as_u64)
}

/// Returns true when the stored access token is still usable (has at least
/// Returns true when the stored access token still has useful life left
/// (not within `NOUS_TOKEN_REFRESH_SKEW_SECONDS` of expiry). Unknown expiry is
/// treated as usable so existing tokens are not needlessly refreshed.
fn oauth_token_is_fresh(tokens: &OAuthStoredTokens) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Prefer the real JWT `exp` claim when the access token carries one.
    if let Some(exp) = jwt_expiry(&tokens.access_token) {
        return exp.saturating_sub(now) >= NOUS_TOKEN_REFRESH_SKEW_SECONDS;
    }
    if tokens.expires_at == 0 {
        return true;
    }
    tokens.expires_at.saturating_sub(now) >= NOUS_TOKEN_REFRESH_SKEW_SECONDS
}

/// Upsert a Nous Portal OAuth profile from a freshly obtained token response.
pub fn oauth_profile_from_token_response(
    _name: impl Into<String>,
    provider: impl Into<String>,
    token: &OAuthTokenResponse,
    portal_base_url: &str,
    inference_base_url: &str,
    client_id: &str,
    scope: &str,
) -> AuthProfile {
    let expires_at = token
        .expires_in
        .map(|secs| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() + secs)
                .unwrap_or(0)
        })
        .unwrap_or(0);
    AuthProfile {
        provider: provider.into(),
        method: AuthMethod::Oauth,
        base_url: Some(inference_base_url.to_string()),
        secret_ref: String::new(),
        disabled: false,
        oauth: Some(OAuthStoredTokens {
            access_token: token.access_token.clone(),
            refresh_token: token.refresh_token.clone().unwrap_or_default(),
            client_id: client_id.to_string(),
            portal_base_url: portal_base_url.to_string(),
            inference_base_url: inference_base_url.to_string(),
            token_type: token.token_type.clone(),
            expires_at,
            scope: scope.to_string(),
        }),
    }
}

impl AuthStore {
    /// Resolve the current access token for an OAuth profile, refreshing it
    /// (and persisting the rotated tokens) when it is near expiry.
    pub async fn resolve_oauth_token(&mut self, name: &str) -> Result<String> {
        let profile = self
            .profiles
            .get(name)
            .ok_or_else(|| Error::MissingConfig {
                key: format!("auth profile '{}'", name),
            })?;
        if profile.method != AuthMethod::Oauth {
            return Err(Error::Config(format!(
                "Auth profile '{}' is not an OAuth profile",
                profile.provider
            )));
        }
        let tokens = profile.oauth.clone().ok_or_else(|| {
            Error::Config(format!(
                "OAuth profile '{}' has no stored tokens",
                profile.provider
            ))
        })?;

        if oauth_token_is_fresh(&tokens) {
            return Ok(tokens.access_token);
        }

        if tokens.refresh_token.is_empty() {
            return Err(Error::Config(
                "Nous Portal access token expired and no refresh token is available. Re-authenticate with: kerux auth login <provider>".to_string(),
            ));
        }

        let refreshed = refresh_nous_access_token(
            &tokens.portal_base_url,
            &tokens.client_id,
            &tokens.refresh_token,
        )
        .await?;

        let new_tokens = OAuthStoredTokens {
            access_token: refreshed.access_token.clone(),
            refresh_token: refreshed
                .refresh_token
                .clone()
                .unwrap_or(tokens.refresh_token),
            client_id: tokens.client_id,
            portal_base_url: tokens.portal_base_url,
            inference_base_url: tokens.inference_base_url,
            token_type: refreshed.token_type.clone(),
            expires_at: refreshed
                .expires_in
                .map(|secs| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() + secs)
                        .unwrap_or(0)
                })
                .unwrap_or(tokens.expires_at),
            scope: tokens.scope,
        };

        if let Some(profile) = self.profiles.get_mut(name) {
            profile.oauth = Some(new_tokens.clone());
        }
        self.save_default()?;

        Ok(new_tokens.access_token)
    }
}

fn fragment_contains_access_token(fragment: &str) -> bool {
    reqwest::Url::parse(&format!("http://127.0.0.1/?{}", fragment))
        .ok()
        .is_some_and(|url| {
            url.query_pairs()
                .any(|(key, _)| key.eq_ignore_ascii_case("access_token"))
        })
}

fn normalize_callback_path(callback_path: &str) -> Result<String> {
    let path = callback_path.trim();
    if !path.starts_with('/') || path.is_empty() {
        return Err(Error::Config(
            "OAuth callback path must start with '/'".to_string(),
        ));
    }
    if path.chars().any(char::is_control) || path.contains('?') || path.contains('#') {
        return Err(Error::Config("Invalid OAuth callback path".to_string()));
    }
    Ok(path.to_string())
}

fn parse_http_callback_request(
    request: &str,
    redirect_uri: &str,
    callback_path: &str,
    expected_state: &str,
) -> Result<String> {
    let request_line = request
        .lines()
        .next()
        .ok_or_else(|| Error::Config("OAuth callback request was empty".to_string()))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| Error::Config("OAuth callback missing method".to_string()))?;
    let target = parts
        .next()
        .ok_or_else(|| Error::Config("OAuth callback missing target".to_string()))?;
    let version = parts
        .next()
        .ok_or_else(|| Error::Config("OAuth callback missing HTTP version".to_string()))?;

    if method != "GET" {
        return Err(Error::Config("OAuth callback must use GET".to_string()));
    }
    if !version.starts_with("HTTP/") {
        return Err(Error::Config(
            "OAuth callback invalid HTTP version".to_string(),
        ));
    }
    if !target.starts_with('/') || target.contains('#') {
        return Err(Error::Config(
            "OAuth callback target must be an origin path".to_string(),
        ));
    }

    let redirect =
        reqwest::Url::parse(redirect_uri).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    let callback_url = format!(
        "{}://{}:{}{}",
        redirect.scheme(),
        redirect.host_str().unwrap_or("127.0.0.1"),
        redirect
            .port()
            .ok_or_else(|| Error::Config("OAuth redirect URI missing port".to_string()))?,
        target
    );
    let parsed =
        reqwest::Url::parse(&callback_url).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    if parsed.path() != callback_path {
        return Err(Error::Config("OAuth callback path mismatch".to_string()));
    }

    parse_loopback_authorization_code(&callback_url, expected_state)
}

async fn write_oauth_callback_response(
    stream: &mut tokio::net::TcpStream,
    success: bool,
) -> std::io::Result<()> {
    let (status, body) = if success {
        (
            "200 OK",
            "Kerux authentication complete. You can close this window.",
        )
    } else {
        (
            "400 Bad Request",
            "Kerux authentication failed. Return to the terminal and try again.",
        )
    };
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await
}

fn validate_loopback_redirect_uri(uri: &str) -> Result<()> {
    let url = reqwest::Url::parse(uri).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    if url.scheme() != "http" {
        return Err(Error::Config(
            "OAuth redirect URI must use loopback http".to_string(),
        ));
    }
    if url.host_str() != Some("127.0.0.1") {
        return Err(Error::Config(
            "OAuth redirect URI must bind to 127.0.0.1".to_string(),
        ));
    }
    if url.port().is_none() {
        return Err(Error::Config(
            "OAuth redirect URI must include a loopback port".to_string(),
        ));
    }
    Ok(())
}

fn validate_oauth_token_endpoint(endpoint: &str, allow_loopback_http: bool) -> Result<()> {
    let url =
        reqwest::Url::parse(endpoint).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    if url.scheme() == "https" {
        return Ok(());
    }
    if allow_loopback_http && url.scheme() == "http" && url.host_str() == Some("127.0.0.1") {
        return Ok(());
    }
    Err(Error::Config(
        "OAuth token endpoint must use https".to_string(),
    ))
}

fn validate_oauth_form_value(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::Config(format!("Invalid OAuth {}", label)));
    }
    Ok(())
}

fn random_base64_url(byte_len: usize) -> Result<String> {
    let mut bytes = vec![0_u8; byte_len];
    getrandom::getrandom(&mut bytes).map_err(|error| {
        Error::Config(format!("Failed to generate secure random bytes: {}", error))
    })?;
    Ok(base64_url_no_pad(&bytes))
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        encoded.push(BASE64_URL_SAFE[(b0 >> 2) as usize] as char);
        encoded.push(BASE64_URL_SAFE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(BASE64_URL_SAFE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        }
        if chunk.len() > 2 {
            encoded.push(BASE64_URL_SAFE[(b2 & 0b0011_1111) as usize] as char);
        }
    }
    encoded
}

fn validate_name(label: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::Config(format!("Invalid {} name", label)));
    }
    Ok(value)
}

fn validate_env_var(value: String) -> Result<String> {
    let value = value.trim().to_string();
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(Error::Config(
            "Invalid environment variable name".to_string(),
        ));
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(Error::Config(
            "Invalid environment variable name".to_string(),
        ));
    }
    if chars.any(|ch| !(ch == '_' || ch.is_ascii_alphanumeric())) {
        return Err(Error::Config(
            "Invalid environment variable name".to_string(),
        ));
    }
    Ok(value)
}

fn validate_base_url(value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().any(char::is_control) {
        return Err(Error::Config("Invalid auth profile base URL".to_string()));
    }
    reqwest::Url::parse(&value).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nous_oauth_client_id_matches_portal_registration() {
        assert_eq!(NOUS_CLIENT_ID, "hermes-cli");
    }

    fn temp_auth_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("kerux_auth_{}_{}", name, std::process::id()))
            .join("auth.json")
    }

    fn cleanup_auth_path(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn auth_store_round_trips_without_secret_values() {
        let path = temp_auth_path("round_trip");
        cleanup_auth_path(&path);
        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile(
                "openai-default",
                "openai",
                "OPENAI_API_KEY",
                Some("https://api.openai.com/v1".to_string()),
            )
            .unwrap();

        store.save_to(&path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("env:OPENAI_API_KEY"));
        assert!(!raw.contains("sk-"));

        let loaded = AuthStore::load_from(&path).unwrap();
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(
            loaded.profiles["openai-default"].resolved_env_var(),
            Some("OPENAI_API_KEY")
        );

        cleanup_auth_path(&path);
    }

    #[test]
    fn bearer_token_profile_round_trips_without_secret_values() {
        let path = temp_auth_path("bearer_round_trip");
        cleanup_auth_path(&path);
        let mut store = AuthStore::default();
        store
            .upsert_bearer_token_env_profile(
                "google-default",
                "google-gemini",
                "GOOGLE_OAUTH_ACCESS_TOKEN",
                Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            )
            .unwrap();

        store.save_to(&path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("bearer_token"));
        assert!(raw.contains("env:GOOGLE_OAUTH_ACCESS_TOKEN"));
        assert!(!raw.contains("ya29."));

        let loaded = AuthStore::load_from(&path).unwrap();
        assert_eq!(
            loaded.profiles["google-default"].method,
            AuthMethod::BearerToken
        );
        assert_eq!(
            loaded.profiles["google-default"].resolved_env_var(),
            Some("GOOGLE_OAUTH_ACCESS_TOKEN")
        );

        cleanup_auth_path(&path);
    }

    #[test]
    fn bearer_token_profiles_require_base_url() {
        let mut store = AuthStore::default();

        let result = store.upsert_bearer_token_env_profile(
            "google-default",
            "google-gemini",
            "GOOGLE_OAUTH_ACCESS_TOKEN",
            None,
        );

        assert!(result.is_err());
    }

    #[test]
    fn resolve_api_key_rejects_bearer_profiles() {
        let mut store = AuthStore::default();
        store
            .upsert_bearer_token_env_profile(
                "google-default",
                "google-gemini",
                "GOOGLE_OAUTH_ACCESS_TOKEN",
                Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            )
            .unwrap();

        assert!(store.resolve_api_key("google-default").is_err());
    }

    #[test]
    fn load_rejects_bearer_profile_without_base_url() {
        let path = temp_auth_path("broken_bearer_load");
        cleanup_auth_path(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
  "version": 1,
  "profiles": {
    "broken-bearer": {
      "provider": "google-gemini",
      "method": "bearer_token",
      "base_url": null,
      "secret_ref": "env:GOOGLE_OAUTH_ACCESS_TOKEN",
      "disabled": false
    }
  }
}"#,
        )
        .unwrap();

        let result = AuthStore::load_from(&path);

        assert!(result.is_err());
        cleanup_auth_path(&path);
    }

    #[test]
    fn resolve_auth_token_rejects_bearer_profile_without_base_url() {
        let profile = AuthProfile {
            provider: "google-gemini".to_string(),
            method: AuthMethod::BearerToken,
            base_url: None,
            secret_ref: "env:GOOGLE_OAUTH_ACCESS_TOKEN".to_string(),
            disabled: false,
            oauth: None,
        };

        assert!(profile.resolve_auth_token().is_err());
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64_url_no_pad(&Sha256::digest(verifier.as_bytes()));

        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_pkce_and_state_are_url_safe() {
        let state = generate_oauth_state().unwrap();
        let pkce = generate_pkce_challenge().unwrap();

        assert_eq!(state.len(), 43);
        assert_eq!(pkce.code_verifier.len(), 43);
        assert_eq!(pkce.code_challenge.len(), 43);
        assert_eq!(pkce.code_challenge_method, "S256");
        assert!(state.chars().all(is_base64_url_char));
        assert!(pkce.code_verifier.chars().all(is_base64_url_char));
        assert!(pkce.code_challenge.chars().all(is_base64_url_char));
    }

    #[test]
    fn authorization_url_requires_loopback_redirect_and_includes_pkce() {
        let pkce = PkceChallenge {
            code_verifier: "verifier".to_string(),
            code_challenge: "challenge".to_string(),
            code_challenge_method: "S256",
        };

        let url = build_oauth_authorization_url(
            "https://accounts.example/authorize",
            "client-id",
            "http://127.0.0.1:49152/callback",
            &["scope-a".to_string(), "scope-b".to_string()],
            "state-value",
            &pkce,
        )
        .unwrap();

        assert!(url.starts_with("https://accounts.example/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client-id"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A49152%2Fcallback"));
        assert!(url.contains("scope=scope-a+scope-b"));
        assert!(url.contains("state=state-value"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));

        let result = build_oauth_authorization_url(
            "http://accounts.example/authorize",
            "client-id",
            "http://127.0.0.1:49152/callback",
            &[],
            "state-value",
            &pkce,
        );
        assert!(result.is_err());

        let result = build_oauth_authorization_url(
            "https://accounts.example/authorize",
            "client-id",
            "http://localhost:49152/callback",
            &[],
            "state-value",
            &pkce,
        );
        assert!(result.is_err());
    }

    #[test]
    fn loopback_callback_validates_state_and_rejects_tokens() {
        let code = parse_loopback_authorization_code(
            "http://127.0.0.1:49152/callback?code=auth-code&state=expected",
            "expected",
        )
        .unwrap();
        assert_eq!(code, "auth-code");

        assert!(parse_loopback_authorization_code(
            "http://127.0.0.1:49152/callback?code=auth-code&state=wrong",
            "expected",
        )
        .is_err());
        assert!(parse_loopback_authorization_code(
            "http://127.0.0.1:49152/callback?access_token=token&state=expected",
            "expected",
        )
        .is_err());
        assert!(parse_loopback_authorization_code(
            "http://127.0.0.1:49152/callback#access_token=token&state=expected",
            "expected",
        )
        .is_err());
        assert!(parse_loopback_authorization_code(
            "http://127.0.0.1:49152/callback#access%5Ftoken=token&state=expected",
            "expected",
        )
        .is_err());
    }

    fn is_base64_url_char(ch: char) -> bool {
        ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'
    }

    #[tokio::test]
    async fn loopback_receiver_accepts_valid_callback_once() {
        let receiver = LoopbackOAuthReceiver::bind("/oauth/callback")
            .await
            .unwrap();
        let redirect_uri = reqwest::Url::parse(receiver.redirect_uri()).unwrap();
        let port = redirect_uri.port().unwrap();

        let wait = tokio::spawn(receiver.wait_for_code("expected-state", Duration::from_secs(5)));
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /oauth/callback?code=auth-code&state=expected-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        let code = wait.await.unwrap().unwrap();
        assert_eq!(code, "auth-code");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
    }

    #[tokio::test]
    async fn loopback_receiver_rejects_wrong_path_and_access_token() {
        let receiver = LoopbackOAuthReceiver::bind("/oauth/callback")
            .await
            .unwrap();
        let redirect_uri = reqwest::Url::parse(receiver.redirect_uri()).unwrap();
        let port = redirect_uri.port().unwrap();

        let wait = tokio::spawn(receiver.wait_for_code("expected-state", Duration::from_secs(5)));
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /oauth/callback?access_token=token&state=expected-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        assert!(wait.await.unwrap().is_err());
        assert!(response.starts_with("HTTP/1.1 400 Bad Request"));

        let receiver = LoopbackOAuthReceiver::bind("/oauth/callback")
            .await
            .unwrap();
        let redirect_uri = reqwest::Url::parse(receiver.redirect_uri()).unwrap();
        let port = redirect_uri.port().unwrap();

        let wait = tokio::spawn(receiver.wait_for_code("expected-state", Duration::from_secs(5)));
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /wrong?code=auth-code&state=expected-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();

        assert!(wait.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn token_exchange_posts_pkce_code_form_and_parses_bearer_token() {
        let mut server = mockito::Server::new_async().await;
        let exchange = server
            .mock("POST", "/token")
            .match_header("content-type", "application/x-www-form-urlencoded")
            .match_body(mockito::Matcher::Regex(
                "grant_type=authorization_code&client_id=client-id&redirect_uri=http%3A%2F%2F127.0.0.1%3A49152%2Fcallback&code=auth-code&code_verifier=verifier".to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "access_token": "access-token",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "scope": "scope-a scope-b"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let client = http_client();

        let token = exchange_oauth_authorization_code_with_client(
            &client,
            &format!("{}/token", server.url()),
            "client-id",
            "http://127.0.0.1:49152/callback",
            "auth-code",
            "verifier",
            true,
        )
        .await
        .unwrap();

        assert_eq!(token.access_token, "access-token");
        assert_eq!(token.token_type, "Bearer");
        assert_eq!(token.expires_in, Some(3600));
        assert_eq!(token.scope.as_deref(), Some("scope-a scope-b"));
        exchange.assert_async().await;
    }

    #[tokio::test]
    async fn token_exchange_rejects_insecure_endpoint_and_non_bearer_response() {
        let result = exchange_oauth_authorization_code(
            "http://accounts.example/token",
            "client-id",
            "http://127.0.0.1:49152/callback",
            "auth-code",
            "verifier",
        )
        .await;
        assert!(result.is_err());

        let mut server = mockito::Server::new_async().await;
        let _exchange = server
            .mock("POST", "/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "access_token": "access-token",
                    "token_type": "mac"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let client = http_client();

        let result = exchange_oauth_authorization_code_with_client(
            &client,
            &format!("{}/token", server.url()),
            "client-id",
            "http://127.0.0.1:49152/callback",
            "auth-code",
            "verifier",
            true,
        )
        .await;

        assert!(result.is_err());
    }

    #[test]
    fn invalid_env_var_names_are_rejected() {
        let mut store = AuthStore::default();

        let result = store.upsert_api_key_env_profile("bad", "openai", "OPENAI API KEY", None);

        assert!(result.is_err());
    }

    #[test]
    fn missing_env_secret_returns_missing_config() {
        let mut store = AuthStore::default();
        let missing_env = format!("KERUX_TEST_MISSING_API_KEY_{}", std::process::id());
        store
            .upsert_api_key_env_profile("missing-secret", "openai", missing_env, None)
            .unwrap();

        let result = store.resolve_api_key("missing-secret");

        assert!(matches!(result, Err(Error::MissingConfig { .. })));
    }

    #[test]
    fn base_url_rejects_control_characters() {
        let mut store = AuthStore::default();

        let result = store.upsert_api_key_env_profile(
            "bad-url",
            "openai",
            "OPENAI_API_KEY",
            Some("https://api.openai.com/v1\nspoof".to_string()),
        );

        assert!(result.is_err());
    }

    #[test]
    fn remove_profile_reports_whether_profile_existed() {
        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile("openai-default", "openai", "OPENAI_API_KEY", None)
            .unwrap();

        assert!(store.remove_profile("openai-default"));
        assert!(!store.remove_profile("openai-default"));
    }

    #[test]
    fn jwt_expiry_decodes_exp_claim() {
        // header.payload.signature with payload {"exp": 1700000000}
        let payload = base64_url_nopad("{\"exp\":1700000000}");
        let token = format!("eyJhbGciOiJIUzI1NiJ9.{}.sig", payload);
        assert_eq!(jwt_expiry(&token), Some(1700000000));
        assert_eq!(jwt_expiry("not-a-jwt"), None);
    }

    fn base64_url_nopad(input: &str) -> String {
        // Manual base64url (no padding) encode using the project's alphabet,
        // mirroring the decode path used by `jwt_expiry`.
        let bytes = input.as_bytes();
        let mut out = String::new();
        let mut buf = 0u32;
        let mut nbits = 0u32;
        for &b in bytes {
            buf = (buf << 8) | b as u32;
            nbits += 8;
            while nbits >= 6 {
                nbits -= 6;
                let idx = ((buf >> nbits) & 0x3f) as usize;
                out.push(BASE64_URL_SAFE[idx] as char);
            }
        }
        if nbits > 0 {
            let idx = ((buf << (6 - nbits)) & 0x3f) as usize;
            out.push(BASE64_URL_SAFE[idx] as char);
        }
        out
    }

    #[tokio::test]
    async fn request_nous_device_code_posts_expected_body() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/oauth/device/code")
            .match_body(mockito::Matcher::Regex("client_id=hermes-cli".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"device_code":"dc123","user_code":"ABCD-EFGH","verification_uri_complete":"https://portal.nousresearch.com/device?code=ABCD-EFGH","expires_in":600,"interval":5}"#,
            )
            .create_async()
            .await;

        let device = request_nous_device_code(&server.url(), NOUS_CLIENT_ID, NOUS_SCOPE)
            .await
            .unwrap();
        assert_eq!(device.device_code, "dc123");
        assert_eq!(device.user_code, "ABCD-EFGH");
        assert!(device.verification_uri_complete.contains("ABCD-EFGH"));
        assert_eq!(device.expires_in, 600);
        assert_eq!(device.interval, 5);
        _m.assert_async().await;
        server.reset();
    }

    #[tokio::test]
    async fn poll_nous_device_token_handles_pending_then_success() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/oauth/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"access-abc","token_type":"Bearer","refresh_token":"refresh-xyz","expires_in":3600}"#,
            )
            .create_async()
            .await;

        let token = poll_nous_device_token(
            &server.url(),
            NOUS_CLIENT_ID,
            "dc123",
            600,
            0, // no delay between polls in test
        )
        .await
        .unwrap();
        assert_eq!(token.access_token, "access-abc");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-xyz"));
        _m.assert_async().await;
        server.reset();
    }

    #[tokio::test]
    async fn refresh_nous_access_token_uses_refresh_grant() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::Regex("grant_type=refresh_token".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"access-new","token_type":"Bearer","refresh_token":"refresh-new","expires_in":3600}"#,
            )
            .create_async()
            .await;

        let token = refresh_nous_access_token(&server.url(), NOUS_CLIENT_ID, "refresh-old")
            .await
            .unwrap();
        assert_eq!(token.access_token, "access-new");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-new"));
        _m.assert_async().await;
        server.reset();
    }

    #[tokio::test]
    async fn resolve_oauth_token_refreshes_when_expired_and_persists() {
        let dir = std::env::temp_dir().join(format!("kerux-auth-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Isolate the default auth store (used by `save_default`/`load_default`
        // inside `resolve_oauth_token`) so the test never writes over the
        // developer's real `~/.kerux/data/auth.json`.
        std::env::set_var("KERUX_HOME", dir.to_str().unwrap());
        let path = dir.join("auth.json");
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::Regex("grant_type=refresh_token".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"access_token":"access-refreshed","token_type":"Bearer","refresh_token":"refresh-2","expires_in":3600}"#,
            )
            .create_async()
            .await;

        // Build an OAuth profile whose access token is already expired (exp in the past).
        let expired_payload = base64_url_nopad("{\"exp\":1}");
        let expired_token = format!("eyJhbGciOiJIUzI1NiJ9.{}.sig", expired_payload);
        assert_eq!(jwt_expiry(&expired_token), Some(1));
        let mut profile = oauth_profile_from_token_response(
            "nous-default",
            "nous",
            &OAuthTokenResponse {
                access_token: expired_token,
                token_type: "Bearer".to_string(),
                refresh_token: Some("refresh-old".to_string()),
                expires_in: Some(3600),
                scope: None,
            },
            &server.url(),
            NOUS_INFERENCE_URL,
            NOUS_CLIENT_ID,
            NOUS_SCOPE,
        );
        // Force expiry to the past regardless of clock skew handling.
        if let Some(oauth) = profile.oauth.as_mut() {
            oauth.expires_at = 1;
        }
        let mut store = AuthStore::default();
        store.profiles.insert("nous-default".to_string(), profile);
        store.save_to(&path).unwrap();

        let mut loaded = AuthStore::load_from(&path).unwrap();
        let access = loaded.resolve_oauth_token("nous-default").await.unwrap();
        assert_eq!(access, "access-refreshed");

        // The refreshed tokens should be persisted on the in-memory store.
        let stored = loaded.profiles["nous-default"].oauth.as_ref().unwrap();
        assert_eq!(stored.access_token, "access-refreshed");
        assert_eq!(stored.refresh_token, "refresh-2");
        _m.assert_async().await;
        server.reset();
    }
}
