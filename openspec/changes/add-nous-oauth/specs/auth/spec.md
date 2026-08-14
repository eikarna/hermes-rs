# Auth capability — Nous Portal OAuth

## ADDED Requirements

### Requirement: Nous Portal device-code login
The CLI MUST provide `hermes auth login nous`, which initiates the Nous Portal
OAuth device-code flow, prints the `verification_uri_complete` for the user to
approve in a browser, polls the token endpoint until approval, and persists an
`OAuth` auth profile named `nous-default` in the auth store.

#### Scenario: successful login
- Given the user runs `hermes auth login nous`
- When they approve the printed device code in the browser
- Then an `OAuth` profile `nous-default` is stored with a non-empty access token,
  a non-empty refresh token, `portal_base_url`, and `inference_base_url`.

#### Scenario: declined or expired
- Given the device code expires or the user declines
- Then the command exits with a non-zero error and no profile is written.

### Requirement: OAuth profile auto-refresh
When a request is built for an `OAuth` auth profile, the runtime MUST resolve the
current access token and, if it is within 120 seconds of JWT `exp` expiry (or already
expired), perform a refresh-token grant, persist the rotated tokens, and use the new
access token as a Bearer credential against `inference_base_url`.

#### Scenario: token still valid
- Given a stored access token with `exp` more than 120s in the future
- When a request is built
- Then the stored access token is used without a network refresh.

#### Scenario: token near expiry
- Given a stored access token within 120s of `exp`
- When a request is built
- Then the token endpoint is called with `grant_type=refresh_token`, the new tokens
  are persisted, and the new access token is used.

### Requirement: `nous` provider
The client configuration MUST accept `provider = "nous"`, which binds to the
OpenAI-compatible Nous inference endpoint and requires no API key (credentials come
from the linked `OAuth` auth profile's access token).

#### Scenario: explicit provider
- Given `[client] provider = "nous"` and `auth_ref = "nous-default"`
- When the client is built
- Then requests target `https://inference-api.nousresearch.com/v1` with
  `Authorization: Bearer <access_token>`.

## MODIFIED Requirements

### Requirement: auth profile validation
`AuthProfile::validate` MUST accept `method = "oauth"` without a `secret_ref`,
provided both `access_token` and `refresh_token` are non-empty and both
`portal_base_url` and `inference_base_url` are set. (ApiKey/BearerToken profiles
continue to require an `env:` secret reference.)

#### Scenario: oauth profile without secret_ref
- Given an `OAuth` profile with both tokens and both base URLs set
- When `validate()` runs
- Then it passes without requiring an `env:` reference.
