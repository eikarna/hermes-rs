//! Small shared persistence helpers for file-backed state.
//!
//! Several subsystems (sessions, todos, memory tools) need the same thing:
//! a data directory under `~/.hermes-rs/`, filesystem-safe keys, and JSON
//! round-trips. This module keeps that logic in one place.

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Root data directory: `~/.hermes-rs/<subdir>`.
///
/// The `HERMES_RS_HOME` env var overrides the `~/.hermes-rs` root entirely
/// (useful for tests and portable installs). Falls back to
/// `./.hermes-rs/<subdir>` when the home dir is unavailable.
pub fn data_root() -> PathBuf {
    if let Some(home) = std::env::var_os("HERMES_RS_HOME") {
        return PathBuf::from(home);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".hermes-rs")
}

/// `<data_root>/<subdir>`.
pub fn data_dir(subdir: &str) -> PathBuf {
    data_root().join(subdir)
}

/// Map an arbitrary key (session id, channel key, memory key) to a
/// filesystem-safe name. Alphanumerics, `-`, and `_` pass through;
/// everything else becomes `_`.
pub fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Write `value` as pretty JSON to `path`, creating parent dirs.
///
/// Atomic: the JSON is written to a sibling temp file first, then renamed
/// over the target. A crash or kill mid-write leaves either the old file or
/// the new one — never a truncated half-file that `read_json` would reject
/// (silently dropping the whole session history).
pub fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp = path.to_path_buf();
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    tmp.set_file_name(tmp_name);

    std::fs::write(&tmp, json)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Don't litter temp files on failure.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Read JSON from `path`. Returns `None` when the file is missing or
/// corrupt — callers treat both as "start fresh" (logged upstream).
pub fn read_json<T: DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Item {
        name: String,
        n: u32,
    }

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize_key("telegram:-100123"), "telegram_-100123");
        assert_eq!(sanitize_key("abc_DEF-123"), "abc_DEF-123");
        assert_eq!(sanitize_key("a/b\\c:d"), "a_b_c_d");
    }

    #[test]
    fn json_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("item.json");
        let item = Item {
            name: "x".into(),
            n: 42,
        };
        write_json(&path, &item).unwrap();
        let loaded: Item = read_json(&path).unwrap();
        assert_eq!(loaded, item);
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(read_json::<Item>(&path).is_none());
    }

    #[test]
    fn read_corrupt_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(read_json::<Item>(&path).is_none());
    }
}
