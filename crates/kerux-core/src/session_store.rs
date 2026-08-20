//! Per-channel session persistence.
//!
//! Stores conversation history as JSON files so the agent remembers context
//! across gateway restarts. One file per channel key (`platform:channel_id`).
//!
//! This is the missing piece that made the bot "forget" everything after a
//! restart: the in-memory `KeruxAgent.conversation` was the only history,
//! and it died with the process.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::client::Message;
use crate::error::{Error, Result};

/// Maximum messages kept per session to bound file size and context.
const MAX_SESSION_MESSAGES: usize = 200;

/// Current on-disk session format version.
///
/// v1 was a bare JSON array of messages. v2 wraps the array in an object
/// that also carries the rolling context summary produced by compaction.
const SESSION_FORMAT_VERSION: u32 = 2;

/// Everything persisted for one channel.
#[derive(Debug, Clone, Default)]
pub struct SessionData {
    /// Rolling summary of compacted-away older messages, if any.
    pub summary: Option<String>,
    /// Recent messages (the live tail of the conversation).
    pub messages: Vec<Message>,
}

/// v2 on-disk representation.
#[derive(Serialize, Deserialize)]
struct SessionFile {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    messages: Vec<Message>,
}

/// File-backed per-channel conversation store.
#[derive(Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// Create a store rooted at `dir` (created lazily on first save).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Default location: `~/.kerux/sessions`.
    pub fn default_dir() -> PathBuf {
        crate::persist::data_dir("sessions")
    }

    /// Map a channel key to a safe file path.
    fn path_for(&self, channel_key: &str) -> PathBuf {
        self.dir.join(format!(
            "{}.json",
            crate::persist::sanitize_key(channel_key)
        ))
    }

    /// Load history for a channel. Returns empty [`SessionData`] when no
    /// session file exists or it is corrupt (corrupt files are logged, not
    /// fatal). Transparently reads both v1 (bare message array) and v2
    /// (object with rolling summary) files.
    pub fn load(&self, channel_key: &str) -> SessionData {
        let path = self.path_for(channel_key);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => return SessionData::default(),
        };
        // v2 first, then fall back to the legacy v1 bare-array format.
        if let Ok(file) = serde_json::from_str::<SessionFile>(&raw) {
            return SessionData {
                summary: file.summary.filter(|s| !s.trim().is_empty()),
                messages: file.messages,
            };
        }
        match serde_json::from_str::<Vec<Message>>(&raw) {
            Ok(msgs) => SessionData {
                summary: None,
                messages: msgs,
            },
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Corrupt session file; starting fresh"
                );
                SessionData::default()
            }
        }
    }

    /// Save history for a channel, capped to the last 200 messages.
    ///
    /// Assistant `reasoning` (chain-of-thought) is stripped before persisting:
    /// it is internal, large, and would otherwise be re-sent on every request.
    ///
    /// Uses the atomic `persist::write_json` so a crash mid-write can never
    /// leave a truncated session file.
    pub fn save(
        &self,
        channel_key: &str,
        summary: Option<&str>,
        messages: &[Message],
    ) -> Result<()> {
        let start = messages.len().saturating_sub(MAX_SESSION_MESSAGES);
        let trimmed: Vec<Message> = messages[start..]
            .iter()
            .map(|m| {
                let mut m = m.clone();
                m.reasoning = None;
                m
            })
            .collect();

        let file = SessionFile {
            version: SESSION_FORMAT_VERSION,
            summary: summary
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            messages: trimmed,
        };

        let path = self.path_for(channel_key);
        crate::persist::write_json(&path, &file).map_err(|e| {
            Error::Config(format!(
                "Failed to write session file '{}': {}",
                path.display(),
                e
            ))
        })?;
        Ok(())
    }

    /// Clear history for a channel (deletes the backing file).
    pub fn clear(&self, channel_key: &str) {
        let path = self.path_for(channel_key);
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Role;

    fn temp_store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (SessionStore::new(dir.path().to_path_buf()), dir)
    }

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            reasoning: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn load_missing_returns_empty() {
        let (store, _dir) = temp_store();
        let data = store.load("telegram:123");
        assert!(data.messages.is_empty());
        assert!(data.summary.is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let (store, _dir) = temp_store();
        let msgs = vec![
            msg(Role::User, "halo"),
            msg(Role::Assistant, "hai juga"),
            msg(Role::User, "apa kabar"),
        ];
        store.save("telegram:123", None, &msgs).unwrap();
        let loaded = store.load("telegram:123");
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.messages[0].content, "halo");
        assert_eq!(loaded.messages[2].content, "apa kabar");
        assert!(loaded.summary.is_none());
    }

    #[test]
    fn save_strips_reasoning() {
        let (store, _dir) = temp_store();
        let mut m = msg(Role::Assistant, "jawab");
        m.reasoning = Some("long chain of thought".to_string());
        store.save("telegram:1", None, &[m]).unwrap();
        let loaded = store.load("telegram:1");
        assert!(loaded.messages[0].reasoning.is_none());
    }

    #[test]
    fn save_caps_history() {
        let (store, _dir) = temp_store();
        let msgs: Vec<Message> = (0..(MAX_SESSION_MESSAGES + 50))
            .map(|i| msg(Role::User, &format!("m{}", i)))
            .collect();
        store.save("telegram:1", None, &msgs).unwrap();
        let loaded = store.load("telegram:1");
        assert_eq!(loaded.messages.len(), MAX_SESSION_MESSAGES);
        // Keeps the tail (most recent).
        assert_eq!(loaded.messages.last().unwrap().content, "m249");
    }

    #[test]
    fn summary_roundtrip() {
        let (store, _dir) = temp_store();
        let msgs = vec![msg(Role::User, "recent")];
        store
            .save("telegram:1", Some("user asked about rust"), &msgs)
            .unwrap();
        let loaded = store.load("telegram:1");
        assert_eq!(loaded.summary.as_deref(), Some("user asked about rust"));
        // Blank summaries are dropped, not persisted.
        store.save("telegram:1", Some("   "), &msgs).unwrap();
        assert!(store.load("telegram:1").summary.is_none());
    }

    #[test]
    fn legacy_v1_bare_array_still_loads() {
        let (store, dir) = temp_store();
        // Hand-write a v1 file: a bare JSON array of messages.
        let v1 = serde_json::json!([
            {"role": "user", "content": "old format"},
            {"role": "assistant", "content": "still readable"}
        ]);
        let path = dir.path().join("telegram_1.json");
        std::fs::write(&path, v1.to_string()).unwrap();
        let loaded = store.load("telegram:1");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].content, "old format");
        assert!(loaded.summary.is_none());
    }

    #[test]
    fn clear_removes_session() {
        let (store, _dir) = temp_store();
        store
            .save("telegram:1", None, &[msg(Role::User, "x")])
            .unwrap();
        assert_eq!(store.load("telegram:1").messages.len(), 1);
        store.clear("telegram:1");
        assert!(store.load("telegram:1").messages.is_empty());
    }

    #[test]
    fn channel_key_sanitized() {
        let (store, _dir) = temp_store();
        // Negative Telegram chat IDs contain '-'; must stay filesystem-safe.
        store
            .save("telegram:-1001234567890", None, &[msg(Role::User, "x")])
            .unwrap();
        assert_eq!(store.load("telegram:-1001234567890").messages.len(), 1);
    }
}
