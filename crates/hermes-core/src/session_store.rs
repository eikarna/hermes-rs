//! Per-channel session persistence.
//!
//! Stores conversation history as JSON files so the agent remembers context
//! across gateway restarts. One file per channel key (`platform:channel_id`).
//!
//! This is the missing piece that made the bot "forget" everything after a
//! restart: the in-memory `HermesAgent.conversation` was the only history,
//! and it died with the process.

use std::path::PathBuf;

use crate::client::Message;
use crate::error::{Error, Result};

/// Maximum messages kept per session to bound file size and context.
const MAX_SESSION_MESSAGES: usize = 200;

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

    /// Default location: `~/.hermes-rs/sessions`.
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

    /// Load history for a channel. Returns an empty vec when no session file
    /// exists or it is corrupt (corrupt files are logged, not fatal).
    pub fn load(&self, channel_key: &str) -> Vec<Message> {
        let path = self.path_for(channel_key);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        match serde_json::from_str::<Vec<Message>>(&raw) {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Corrupt session file; starting fresh"
                );
                Vec::new()
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
    pub fn save(&self, channel_key: &str, messages: &[Message]) -> Result<()> {
        let start = messages.len().saturating_sub(MAX_SESSION_MESSAGES);
        let trimmed: Vec<Message> = messages[start..]
            .iter()
            .map(|m| {
                let mut m = m.clone();
                m.reasoning = None;
                m
            })
            .collect();

        let path = self.path_for(channel_key);
        crate::persist::write_json(&path, &trimmed).map_err(|e| {
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
        assert!(store.load("telegram:123").is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let (store, _dir) = temp_store();
        let msgs = vec![
            msg(Role::User, "halo"),
            msg(Role::Assistant, "hai juga"),
            msg(Role::User, "apa kabar"),
        ];
        store.save("telegram:123", &msgs).unwrap();
        let loaded = store.load("telegram:123");
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].content, "halo");
        assert_eq!(loaded[2].content, "apa kabar");
    }

    #[test]
    fn save_strips_reasoning() {
        let (store, _dir) = temp_store();
        let mut m = msg(Role::Assistant, "jawab");
        m.reasoning = Some("long chain of thought".to_string());
        store.save("telegram:1", &[m]).unwrap();
        let loaded = store.load("telegram:1");
        assert!(loaded[0].reasoning.is_none());
    }

    #[test]
    fn save_caps_history() {
        let (store, _dir) = temp_store();
        let msgs: Vec<Message> = (0..(MAX_SESSION_MESSAGES + 50))
            .map(|i| msg(Role::User, &format!("m{}", i)))
            .collect();
        store.save("telegram:1", &msgs).unwrap();
        let loaded = store.load("telegram:1");
        assert_eq!(loaded.len(), MAX_SESSION_MESSAGES);
        // Keeps the tail (most recent).
        assert_eq!(loaded.last().unwrap().content, "m249");
    }

    #[test]
    fn clear_removes_session() {
        let (store, _dir) = temp_store();
        store.save("telegram:1", &[msg(Role::User, "x")]).unwrap();
        assert_eq!(store.load("telegram:1").len(), 1);
        store.clear("telegram:1");
        assert!(store.load("telegram:1").is_empty());
    }

    #[test]
    fn channel_key_sanitized() {
        let (store, _dir) = temp_store();
        // Negative Telegram chat IDs contain '-'; must stay filesystem-safe.
        store
            .save("telegram:-1001234567890", &[msg(Role::User, "x")])
            .unwrap();
        assert_eq!(store.load("telegram:-1001234567890").len(), 1);
    }
}
