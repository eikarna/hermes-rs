//! Pure secret-redaction primitives for data that may be persisted.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Marker used in place of sensitive values.
pub const REDACTED: &str = "[REDACTED]";

static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bbearer[ \t]+[A-Za-z0-9._~+/=-]+")
        .expect("the bearer redaction regex is valid")
});

static PREFIXED_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{8,}|gh[opurs]_[A-Za-z0-9_]{8,})\b")
        .expect("the prefixed-key redaction regex is valid")
});

/// Redact well-known credential shapes embedded in otherwise unstructured text.
///
/// Patterns are deliberately conservative: generic words such as `token` and
/// `secret` are not sufficient evidence that surrounding text is a credential.
pub fn redact_text(text: &str) -> String {
    let without_bearer = BEARER_RE.replace_all(text, REDACTED);
    PREFIXED_KEY_RE
        .replace_all(&without_bearer, REDACTED)
        .into_owned()
}

/// Return a recursively redacted clone of a JSON value.
///
/// The input is borrowed and remains unchanged.
pub fn redact_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(redact_object(object)),
        Value::Array(items) => Value::Array(items.iter().map(redact_json).collect()),
        Value::String(text) => Value::String(redact_text(text)),
        _ => value.clone(),
    }
}

/// A redacted serialized payload plus integrity and truncation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedPayload {
    /// Redacted payload content, possibly shortened.
    pub content: String,
    /// Byte length after redaction but before truncation.
    pub original_bytes: usize,
    /// SHA-256 of the complete redacted payload.
    pub sha256: String,
    /// Whether `content` omits bytes from the complete redacted payload.
    pub truncated: bool,
}

impl BoundedPayload {
    /// Redact and compactly serialize JSON before applying a byte limit.
    pub fn from_json(value: &Value, max_bytes: usize) -> Result<Self, serde_json::Error> {
        let serialized = serde_json::to_string(&redact_json(value))?;
        Ok(Self::from_redacted_text(&serialized, max_bytes))
    }

    fn from_redacted_text(text: &str, max_bytes: usize) -> Self {
        let truncated = text.len() > max_bytes;
        Self {
            content: if truncated {
                truncate_utf8(text, max_bytes)
            } else {
                text.to_string()
            },
            original_bytes: text.len(),
            sha256: sha256_hex(text.as_bytes()),
            truncated,
        }
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    const ELLIPSIS: &str = "…";

    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes < ELLIPSIS.len() {
        return ".".repeat(max_bytes);
    }

    let available = max_bytes - ELLIPSIS.len();
    let prefix_budget = available.div_ceil(2);
    let suffix_budget = available - prefix_budget;

    let mut prefix_end = prefix_budget.min(text.len());
    while !text.is_char_boundary(prefix_end) {
        prefix_end -= 1;
    }

    let mut suffix_start = text.len().saturating_sub(suffix_budget);
    while suffix_start < text.len() && !text.is_char_boundary(suffix_start) {
        suffix_start += 1;
    }

    format!(
        "{}{}{}",
        &text[..prefix_end],
        ELLIPSIS,
        &text[suffix_start..]
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn redact_object(object: &Map<String, Value>) -> Map<String, Value> {
    object
        .iter()
        .map(|(key, value)| {
            let value = if is_sensitive_key(key) {
                Value::String(REDACTED.to_string())
            } else {
                redact_json(value)
            };
            (key.clone(), value)
        })
        .collect()
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "authorization"
            | "api_key"
            | "apikey"
            | "token"
            | "access_token"
            | "refresh_token"
            | "password"
            | "secret"
            | "cookie"
            | "set-cookie"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn redacts_sensitive_keys_recursively() {
        let input = json!({
            "authorization": "Bearer top-secret",
            "nested": {
                "password": "hunter2",
                "items": [{"api_key": "sk-test-secret"}]
            },
            "safe": "visible"
        });

        let redacted = super::redact_json(&input);

        assert_eq!(redacted["authorization"], "[REDACTED]");
        assert_eq!(redacted["nested"]["password"], "[REDACTED]");
        assert_eq!(redacted["nested"]["items"][0]["api_key"], "[REDACTED]");
        assert_eq!(redacted["safe"], "visible");
    }

    #[test]
    fn sensitive_keys_are_case_insensitive_and_exact() {
        let input = json!({
            "Authorization": "Basic abc123",
            "APIKEY": "secret-a",
            "token": "secret-b",
            "access_token": "secret-c",
            "refresh_token": "secret-d",
            "secret": "secret-e",
            "Cookie": "session=secret-f",
            "Set-Cookie": "session=secret-g",
            "token_budget": 4096
        });

        let redacted = super::redact_json(&input);

        for key in [
            "Authorization",
            "APIKEY",
            "token",
            "access_token",
            "refresh_token",
            "secret",
            "Cookie",
            "Set-Cookie",
        ] {
            assert_eq!(redacted[key], "[REDACTED]", "key {key} was not redacted");
        }
        assert_eq!(redacted["token_budget"], 4096);
    }

    #[test]
    fn redacts_bearer_values_and_recognized_key_prefixes_in_text() {
        let input = "curl -H 'Authorization: Bearer abc.def-123456' \
                     --key sk-example_1234567890 \
                     --github ghp_Example1234567890";

        let redacted = super::redact_text(input);

        assert!(!redacted.contains("abc.def-123456"));
        assert!(!redacted.contains("sk-example_1234567890"));
        assert!(!redacted.contains("ghp_Example1234567890"));
        assert_eq!(redacted.matches("[REDACTED]").count(), 3);
    }

    #[test]
    fn redacts_single_character_bearer_credentials() {
        let input = "Authorization: Bearer x";

        let redacted = super::redact_text(input);

        assert_eq!(redacted, "Authorization: [REDACTED]");
    }

    #[test]
    fn redacts_string_values_without_over_redacting_source_code() {
        let input = json!({
            "command": "curl -H 'Authorization: bearer abcdef123456' https://example.test",
            "source": "let token_budget = estimate_tokens(message);",
            "label": "secret handling"
        });
        let original = input.clone();

        let once = super::redact_json(&input);
        let twice = super::redact_json(&once);

        assert!(!once["command"].as_str().unwrap().contains("abcdef123456"));
        assert_eq!(once["source"], input["source"]);
        assert_eq!(once["label"], input["label"]);
        assert_eq!(input, original, "redaction mutated the borrowed input");
        assert_eq!(once, twice, "redaction must be idempotent");
    }

    #[test]
    fn bounded_json_preserves_small_redacted_payload() {
        let input = json!({"token": "do-not-persist", "answer": 42});

        let bounded = super::BoundedPayload::from_json(&input, 1024).unwrap();

        assert!(!bounded.truncated);
        assert_eq!(bounded.content, r#"{"answer":42,"token":"[REDACTED]"}"#);
        assert_eq!(bounded.original_bytes, bounded.content.len());
        assert_eq!(bounded.sha256.len(), 64);
        assert!(!bounded.content.contains("do-not-persist"));
    }

    #[test]
    fn bounded_json_truncates_utf8_within_the_byte_limit() {
        let input = json!({"text": "awal-αβγδεζηθικλμνξοπρστυφχψω-akhir"});

        let bounded = super::BoundedPayload::from_json(&input, 32).unwrap();

        assert!(bounded.truncated);
        assert!(
            bounded.content.len() <= 32,
            "got {} bytes",
            bounded.content.len()
        );
        assert!(bounded.original_bytes > bounded.content.len());
        assert!(bounded.content.contains('…'));
        assert_eq!(bounded.sha256.len(), 64);
    }

    #[test]
    fn bounded_json_redacts_before_truncating_across_a_secret_boundary() {
        let secret = "abcdef1234567890";
        let command = format!("{} Bearer {secret} {}", "a".repeat(40), "z".repeat(40));
        let input = json!({"command": command});

        let bounded = super::BoundedPayload::from_json(&input, 40).unwrap();
        let complete = super::BoundedPayload::from_json(&input, 4096).unwrap();

        assert!(bounded.truncated);
        assert!(bounded.content.len() <= 40);
        assert!(!bounded.content.contains(secret));
        assert!(!complete.content.contains(secret));
        assert_eq!(bounded.sha256, complete.sha256);
        assert_eq!(bounded.original_bytes, complete.original_bytes);
    }
}
