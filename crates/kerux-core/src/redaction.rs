//! Pure secret-redaction primitives for data that may be persisted.

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

/// Marker used in place of sensitive values.
pub const REDACTED: &str = "[REDACTED]";

static BEARER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bbearer[ \t]+[A-Za-z0-9._~+/=-]{8,}")
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
}
