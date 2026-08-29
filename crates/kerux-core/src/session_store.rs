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

/// Metadata summary of a stored session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    /// Unique session identifier (the channel key / sanitized file stem).
    pub id: String,
    /// Unix timestamp in seconds (derived from file modified time, or 0 if unavailable).
    pub updated_at: u64,
    /// Message count in the session.
    pub message_count: usize,
    /// Estimated total token count (estimated roughly ~4 chars per token).
    pub estimated_tokens: usize,
    /// Model used (if identifiable from messages, or fallback).
    pub model: Option<String>,
    /// Title or excerpt from the initial user query / summary.
    pub title: String,
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

    /// Return the underlying storage directory.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
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

    /// List all sessions stored in the directory.
    pub fn list(&self) -> Vec<SessionInfo> {
        let mut result = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return result,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let file_stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(stem) => stem.to_string(),
                None => continue,
            };

            let metadata = std::fs::metadata(&path).ok();
            let updated_at = metadata
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let session_data = self.load(&file_stem);
            let message_count = session_data.messages.len();

            let mut total_chars = 0;
            let mut title = String::new();

            for msg in &session_data.messages {
                total_chars += msg.content.len();
                if title.is_empty() && msg.role == crate::client::Role::User {
                    let first_line = msg.content.lines().next().unwrap_or("").trim();
                    if !first_line.is_empty() {
                        title = first_line.chars().take(60).collect();
                    }
                }
            }

            if title.is_empty() {
                if let Some(summary) = &session_data.summary {
                    let first_line = summary.lines().next().unwrap_or("").trim();
                    if !first_line.is_empty() {
                        title = first_line.chars().take(60).collect();
                    }
                }
            }

            if title.is_empty() {
                title = "(empty session)".to_string();
            }

            let estimated_tokens = total_chars.div_ceil(4);

            result.push(SessionInfo {
                id: file_stem,
                updated_at,
                message_count,
                estimated_tokens,
                model: None,
                title,
            });
        }

        // Sort newest first
        result.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        result
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
            images: Vec::new(),
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

    #[test]
    fn list_sessions_orders_by_recency() {
        let (store, _dir) = temp_store();
        store
            .save("session_a", None, &[msg(Role::User, "First question in A")])
            .unwrap();
        store
            .save(
                "session_b",
                None,
                &[msg(Role::User, "Second question in B")],
            )
            .unwrap();

        let list = store.list();
        assert_eq!(list.len(), 2);
        let ids: Vec<_> = list.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"session_a"));
        assert!(ids.contains(&"session_b"));
    }
}
