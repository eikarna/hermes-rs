//! Memory and session management for Hermes-RS
//!
//! Provides persistent memory storage with session history,
//! user profiles, and context block injection.

use crate::client::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// A memory block that can be injected into context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlock {
    /// Unique identifier for this memory
    pub id: String,
    /// Type of memory (e.g., "user_profile", "session_summary", "fact")
    pub block_type: String,
    /// The actual content
    pub content: String,
    /// Importance score (0-100)
    pub importance: u8,
    /// When this memory was created
    pub created_at: i64,
    /// When this memory was last accessed
    pub last_accessed: i64,
    /// Tags for categorization
    pub tags: Vec<String>,
}

impl MemoryBlock {
    /// Create a new memory block
    pub fn new(
        id: impl Into<String>,
        block_type: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            id: id.into(),
            block_type: block_type.into(),
            content: content.into(),
            importance: 50,
            created_at: now,
            last_accessed: now,
            tags: Vec::new(),
        }
    }

    /// Set importance score
    pub fn importance(mut self, score: u8) -> Self {
        self.importance = score.min(100);
        self
    }

    /// Add tags
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Update last accessed time
    pub fn touch(&mut self) {
        self.last_accessed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
    }
}

/// User profile stored in memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    /// User's unique identifier
    pub user_id: String,
    /// User's display name
    pub name: Option<String>,
    /// User's preferences
    pub preferences: HashMap<String, String>,
    /// Known facts about the user
    pub facts: Vec<MemoryBlock>,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            user_id: "default".to_string(),
            name: None,
            preferences: HashMap::new(),
            facts: Vec::new(),
        }
    }
}

/// Session information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier
    pub id: String,
    /// Session title (auto-generated or user-provided)
    pub title: String,
    /// Creation timestamp
    pub created_at: i64,
    /// Last activity timestamp
    pub last_activity: i64,
    /// Message count in this session
    pub message_count: usize,
    /// Total tokens used
    pub total_tokens: usize,
    /// Whether this session is archived
    pub archived: bool,
}

impl Session {
    /// Create a new session
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            id: id.into(),
            title: title.into(),
            created_at: now,
            last_activity: now,
            message_count: 0,
            total_tokens: 0,
            archived: false,
        }
    }

    /// Update last activity timestamp
    pub fn touch(&mut self) {
        self.last_activity = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
    }

    /// Increment message count
    pub fn add_message(&mut self, tokens: usize) {
        self.message_count += 1;
        self.total_tokens += tokens;
        self.touch();
    }
}

/// File-backed storage for persisting memories to MEMORY.md and user profiles to USER.md,
/// using the `§` (section sign) delimiter between entries to match the Python hermes-agent format.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    /// Directory containing MEMORY.md and USER.md
    pub dir: PathBuf,
}

impl MemoryStore {
    /// Create a new MemoryStore pointing at the given directory
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Path to MEMORY.md
    pub fn memory_path(&self) -> PathBuf {
        self.dir.join("MEMORY.md")
    }

    /// Path to USER.md
    pub fn user_path(&self) -> PathBuf {
        self.dir.join("USER.md")
    }

    /// Serialize memories to MEMORY.md format using `§` delimiter.
    ///
    /// Format:
    /// ```text
    /// § [id: test1, type: fact, importance: 80, tags: geography,facts, created_at: 123, last_accessed: 456]
    /// Paris is the capital of France
    /// ```
    pub fn serialize_memories(memories: &HashMap<String, MemoryBlock>) -> String {
        let mut out = String::new();
        // Sort by id for deterministic output
        let mut entries: Vec<_> = memories.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));

        for (_, block) in entries {
            out.push_str(&format!(
                "\u{00A7} [id: {}, type: {}, importance: {}",
                block.id, block.block_type, block.importance
            ));
            if !block.tags.is_empty() {
                out.push_str(&format!(", tags: {}", block.tags.join(",")));
            }
            out.push_str(&format!(
                ", created_at: {}, last_accessed: {}]\n",
                block.created_at, block.last_accessed
            ));
            out.push_str(&block.content);
            out.push('\n');
        }
        out
    }

    /// Parse MEMORY.md content into memory blocks
    pub fn deserialize_memories(content: &str) -> HashMap<String, MemoryBlock> {
        let mut memories = HashMap::new();
        // Split by § character
        let sections: Vec<&str> = content.split('\u{00A7}').collect();

        for section in sections {
            let section = section.trim();
            if section.is_empty() {
                continue;
            }

            let header_start = match section.find('[') {
                Some(pos) => pos,
                None => continue,
            };
            let header_end = match section.find(']') {
                Some(pos) => pos,
                None => continue,
            };

            let header = &section[header_start + 1..header_end];
            let body = section[header_end + 1..].trim().to_string();

            let mut id = String::new();
            let mut block_type = String::new();
            let mut importance: u8 = 50;
            let mut tags: Vec<String> = Vec::new();
            let mut created_at: i64 = 0;
            let mut last_accessed: i64 = 0;

            // Split by ", " (comma-space) to separate fields.
            // Tag values use "," without space, so they stay intact.
            for part in header.split(", ") {
                let part = part.trim();
                if let Some(colon_pos) = part.find(": ") {
                    let key = part[..colon_pos].trim();
                    let value = &part[colon_pos + 2..];
                    match key {
                        "id" => id = value.to_string(),
                        "type" => block_type = value.to_string(),
                        "importance" => importance = value.parse().unwrap_or(50),
                        "tags" => {
                            tags = value
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                        }
                        "created_at" => created_at = value.parse().unwrap_or(0),
                        "last_accessed" => last_accessed = value.parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }

            if id.is_empty() {
                continue;
            }

            let block = MemoryBlock {
                id: id.clone(),
                block_type,
                content: body,
                importance,
                created_at,
                last_accessed,
                tags,
            };
            memories.insert(id, block);
        }

        memories
    }

    /// Write memories to MEMORY.md
    pub fn write_memories(&self, memories: &HashMap<String, MemoryBlock>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let content = Self::serialize_memories(memories);
        std::fs::write(self.memory_path(), content)
    }

    /// Read memories from MEMORY.md
    pub fn read_memories(&self) -> std::io::Result<HashMap<String, MemoryBlock>> {
        let content = std::fs::read_to_string(self.memory_path())?;
        Ok(Self::deserialize_memories(&content))
    }

    /// Serialize user profiles to USER.md format using `§` delimiter.
    ///
    /// Format:
    /// ```text
    /// § [user_id: user1, name: John Doe]
    /// Preferences:
    ///   theme: dark
    ///   language: en
    /// Facts:
    ///   [fact] Likes coding
    /// ```
    pub fn serialize_profiles(profiles: &HashMap<String, UserProfile>) -> String {
        let mut out = String::new();
        // Sort by user_id for deterministic output
        let mut entries: Vec<_> = profiles.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));

        for (_, profile) in entries {
            out.push_str(&format!("\u{00A7} [user_id: {}", profile.user_id));
            if let Some(ref name) = profile.name {
                out.push_str(&format!(", name: {}", name));
            }
            out.push_str("]\n");

            if !profile.preferences.is_empty() {
                out.push_str("Preferences:\n");
                let mut prefs: Vec<_> = profile.preferences.iter().collect();
                prefs.sort_by(|a, b| a.0.cmp(b.0));
                for (key, value) in prefs {
                    out.push_str(&format!("  {}: {}\n", key, value));
                }
            }

            if !profile.facts.is_empty() {
                out.push_str("Facts:\n");
                for fact in &profile.facts {
                    out.push_str(&format!("  [{}] {}\n", fact.block_type, fact.content));
                }
            }
        }
        out
    }

    /// Parse USER.md content into user profiles
    pub fn deserialize_profiles(content: &str) -> HashMap<String, UserProfile> {
        let mut profiles = HashMap::new();
        let sections: Vec<&str> = content.split('\u{00A7}').collect();

        for section in sections {
            let section = section.trim();
            if section.is_empty() {
                continue;
            }

            let header_start = match section.find('[') {
                Some(pos) => pos,
                None => continue,
            };
            let header_end = match section.find(']') {
                Some(pos) => pos,
                None => continue,
            };

            let header = &section[header_start + 1..header_end];
            let body = section[header_end + 1..].trim();

            let mut user_id = String::new();
            let mut name: Option<String> = None;

            for part in header.split(", ") {
                let part = part.trim();
                if let Some(colon_pos) = part.find(": ") {
                    let key = part[..colon_pos].trim();
                    let value = part[colon_pos + 2..].trim();
                    match key {
                        "user_id" => user_id = value.to_string(),
                        "name" => name = Some(value.to_string()),
                        _ => {}
                    }
                }
            }

            if user_id.is_empty() {
                continue;
            }

            let mut preferences = HashMap::new();
            let mut facts = Vec::new();
            let mut current_section = "";

            for line in body.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "Preferences:" {
                    current_section = "preferences";
                } else if trimmed == "Facts:" {
                    current_section = "facts";
                } else if !current_section.is_empty() {
                    match current_section {
                        "preferences" => {
                            if let Some(colon_pos) = trimmed.find(": ") {
                                let key = trimmed[..colon_pos].to_string();
                                let value = trimmed[colon_pos + 2..].to_string();
                                preferences.insert(key, value);
                            }
                        }
                        "facts" if trimmed.starts_with('[') => {
                            if let Some(bracket_end) = trimmed.find(']') {
                                let fact_type = trimmed[1..bracket_end].to_string();
                                let fact_content = trimmed[bracket_end + 1..].trim().to_string();
                                facts.push(MemoryBlock::new(
                                    format!("{}_{}", user_id, facts.len()),
                                    fact_type,
                                    fact_content,
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }

            let profile = UserProfile {
                user_id: user_id.clone(),
                name,
                preferences,
                facts,
            };
            profiles.insert(user_id, profile);
        }

        profiles
    }

    /// Write profiles to USER.md
    pub fn write_profiles(&self, profiles: &HashMap<String, UserProfile>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let content = Self::serialize_profiles(profiles);
        std::fs::write(self.user_path(), content)
    }

    /// Read profiles from USER.md
    pub fn read_profiles(&self) -> std::io::Result<HashMap<String, UserProfile>> {
        let content = std::fs::read_to_string(self.user_path())?;
        Ok(Self::deserialize_profiles(&content))
    }
}

/// Memory manager for storing and retrieving memories
#[derive(Debug, Clone)]
pub struct MemoryManager {
    /// Long-term memories
    long_term: Arc<RwLock<HashMap<String, MemoryBlock>>>,
    /// User profiles
    profiles: Arc<RwLock<HashMap<String, UserProfile>>>,
    /// Sessions
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    /// Current session ID
    current_session: Arc<RwLock<Option<String>>>,
    /// Current session messages
    session_messages: Arc<RwLock<Vec<Message>>>,
    /// Optional directory for file-backed persistent storage (MEMORY.md / USER.md)
    storage_dir: Option<PathBuf>,
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryManager {
    /// Create a new in-memory-only memory manager (no file persistence)
    pub fn new() -> Self {
        Self {
            long_term: Arc::new(RwLock::new(HashMap::new())),
            profiles: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            current_session: Arc::new(RwLock::new(None)),
            session_messages: Arc::new(RwLock::new(Vec::new())),
            storage_dir: None,
        }
    }

    /// Create a new memory manager with file-backed storage in the given directory.
    /// Memories persist to `MEMORY.md` and user profiles to `USER.md` inside `dir`.
    pub fn with_storage_dir(dir: PathBuf) -> Self {
        Self {
            long_term: Arc::new(RwLock::new(HashMap::new())),
            profiles: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            current_session: Arc::new(RwLock::new(None)),
            session_messages: Arc::new(RwLock::new(Vec::new())),
            storage_dir: Some(dir),
        }
    }

    /// Start a new session
    pub async fn start_session(&self, title: impl Into<String>) -> String {
        let session_id = format!("session_{}", uuid_simple());
        let session = Session::new(&session_id, title);

        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session);
        *self.current_session.write().await = Some(session_id.clone());
        self.session_messages.write().await.clear();

        info!(session_id = %session_id, "Started new session");
        session_id
    }

    /// Get or create a session by ID
    pub async fn get_or_create_session(&self, session_id: &str, title: &str) -> Session {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get(session_id) {
            return session.clone();
        }
        let session = Session::new(session_id, title);
        sessions.insert(session_id.to_string(), session.clone());
        session
    }

    /// Add a message to the current session
    pub async fn add_message(&self, message: Message) {
        if let Some(session_id) = self.current_session.read().await.clone() {
            let tokens = crate::context::estimate_message_tokens(&message);
            self.session_messages.write().await.push(message);

            if let Some(session) = self.sessions.write().await.get_mut(&session_id) {
                session.add_message(tokens);
            }
        }
    }

    /// Get messages from the current session
    pub async fn get_session_messages(&self) -> Vec<Message> {
        self.session_messages.read().await.clone()
    }

    /// Get messages from a specific session
    pub async fn get_session_messages_by_id(&self, session_id: &str) -> Vec<Message> {
        // In a real implementation, this would load from persistent storage
        // For now, we only have the current session in memory
        if let Some(current) = self.current_session.read().await.clone() {
            if current == session_id {
                return self.session_messages.read().await.clone();
            }
        }
        Vec::new()
    }

    /// Store a memory block.
    /// When `storage_dir` is set, automatically persists to MEMORY.md on disk.
    pub async fn store(&self, block: MemoryBlock) {
        debug!(block_id = %block.id, block_type = %block.block_type, "Storing memory block");
        self.long_term.write().await.insert(block.id.clone(), block);
        if self.storage_dir.is_some() {
            let _ = self.save_to_disk().await;
        }
    }

    /// Retrieve a memory block by ID
    pub async fn get(&self, id: &str) -> Option<MemoryBlock> {
        let mut block = self.long_term.read().await.get(id).cloned();
        if let Some(ref mut b) = block {
            b.touch();
        }
        block
    }

    /// Search memories by content or tags
    pub async fn search(&self, query: &str) -> Vec<MemoryBlock> {
        let query_lower = query.to_lowercase();
        let memories = self.long_term.read().await;

        memories
            .values()
            .filter(|m| {
                m.content.to_lowercase().contains(&query_lower)
                    || m.tags
                        .iter()
                        .any(|t| t.to_lowercase().contains(&query_lower))
            })
            .cloned()
            .collect()
    }

    /// Get memories by type
    pub async fn get_by_type(&self, block_type: &str) -> Vec<MemoryBlock> {
        self.long_term
            .read()
            .await
            .values()
            .filter(|m| m.block_type == block_type)
            .cloned()
            .collect()
    }

    /// Get high-importance memories
    pub async fn get_important(&self, min_importance: u8) -> Vec<MemoryBlock> {
        self.long_term
            .read()
            .await
            .values()
            .filter(|m| m.importance >= min_importance)
            .cloned()
            .collect()
    }

    /// Store or update a user profile.
    /// When `storage_dir` is set, automatically persists to USER.md on disk.
    pub async fn save_profile(&self, profile: UserProfile) {
        debug!(user_id = %profile.user_id, "Saving user profile");
        self.profiles
            .write()
            .await
            .insert(profile.user_id.clone(), profile);
        if self.storage_dir.is_some() {
            let _ = self.save_to_disk().await;
        }
    }

    /// Get a user profile
    pub async fn get_profile(&self, user_id: &str) -> Option<UserProfile> {
        self.profiles.read().await.get(user_id).cloned()
    }

    /// Save all memories and profiles to disk (MEMORY.md and USER.md).
    /// Returns `Ok(())` immediately if no `storage_dir` is configured.
    pub async fn save_to_disk(&self) -> std::io::Result<()> {
        let dir = match &self.storage_dir {
            Some(dir) => dir,
            None => return Ok(()),
        };
        let store = MemoryStore::new(dir.clone());

        let memories = self.long_term.read().await.clone();
        let profiles = self.profiles.read().await.clone();

        tokio::task::spawn_blocking(move || {
            store.write_memories(&memories)?;
            store.write_profiles(&profiles)?;
            Ok::<(), std::io::Error>(())
        })
        .await
        .unwrap()?;

        Ok(())
    }

    /// Load memories and profiles from disk (MEMORY.md and USER.md).
    /// Returns `Ok(())` immediately if no `storage_dir` is configured.
    /// Missing files are silently skipped.
    pub async fn load_from_disk(&self) -> std::io::Result<()> {
        let dir = match &self.storage_dir {
            Some(dir) => dir,
            None => return Ok(()),
        };
        let store = MemoryStore::new(dir.clone());

        if store.memory_path().exists() {
            let memories = store.read_memories()?;
            *self.long_term.write().await = memories;
        }
        if store.user_path().exists() {
            let profiles = store.read_profiles()?;
            *self.profiles.write().await = profiles;
        }

        Ok(())
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Vec<Session> {
        self.sessions
            .read()
            .await
            .values()
            .filter(|s| !s.archived)
            .cloned()
            .collect()
    }

    /// Archive a session
    pub async fn archive_session(&self, session_id: &str) {
        if let Some(session) = self.sessions.write().await.get_mut(session_id) {
            session.archived = true;
        }
    }

    /// Delete a session
    pub async fn delete_session(&self, session_id: &str) {
        self.sessions.write().await.remove(session_id);
    }

    /// Build context from memory (for injection into prompts)
    pub async fn build_memory_context(&self, max_tokens: usize) -> String {
        let important_memories = self.get_important(70).await;
        let mut context = String::new();
        let mut tokens_used = 0;

        for memory in important_memories {
            let memory_text = format!("[{}] {}\n", memory.block_type, memory.content);
            let memory_tokens = crate::context::estimate_tokens(&memory_text);

            if tokens_used + memory_tokens > max_tokens {
                break;
            }

            context.push_str(&memory_text);
            tokens_used += memory_tokens;
        }

        context
    }

    /// Clear all memories (use with caution)
    pub async fn clear_all(&self) {
        self.long_term.write().await.clear();
        self.profiles.write().await.clear();
        self.sessions.write().await.clear();
        self.session_messages.write().await.clear();
        *self.current_session.write().await = None;
    }
}

/// Generate a simple UUID-like string
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}-{:x}", now.as_secs(), now.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_memory_manager_new() {
        let manager = MemoryManager::new();
        assert!(manager.long_term.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_store_and_get() {
        let manager = MemoryManager::new();
        let block = MemoryBlock::new("test1", "fact", "Paris is the capital of France")
            .importance(80)
            .tags(vec!["geography".to_string(), "facts".to_string()]);

        manager.store(block).await;

        let retrieved = manager.get("test1").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().content, "Paris is the capital of France");
    }

    #[tokio::test]
    async fn test_search() {
        let manager = MemoryManager::new();

        manager
            .store(MemoryBlock::new("1", "fact", "Rivers flow downhill"))
            .await;
        manager
            .store(MemoryBlock::new("2", "fact", "The sky is blue"))
            .await;
        manager
            .store(MemoryBlock::new(
                "3",
                "preference",
                "User prefers dark mode",
            ))
            .await;

        let results = manager.search("river").await;
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("Rivers"));

        let results = manager.search("blue").await;
        assert_eq!(results.len(), 1);

        let results = manager.search("mode").await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_session() {
        let manager = MemoryManager::new();

        let session_id = manager.start_session("Test Session").await;
        assert!(!session_id.is_empty());

        manager.add_message(Message::user("Hello")).await;
        manager.add_message(Message::assistant("Hi there!")).await;

        let messages = manager.get_session_messages().await;
        assert_eq!(messages.len(), 2);
    }

    #[tokio::test]
    async fn test_user_profile() {
        let manager = MemoryManager::new();

        let profile = UserProfile {
            user_id: "user1".to_string(),
            name: Some("John Doe".to_string()),
            preferences: HashMap::new(),
            facts: Vec::new(),
        };

        manager.save_profile(profile).await;

        let retrieved = manager.get_profile("user1").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, Some("John Doe".to_string()));
    }

    // -----------------------------------------------------------------------
    // File-backed persistence tests
    // -----------------------------------------------------------------------

    /// Create a unique temp directory for a test
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hermes_test_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Clean up a test directory
    fn cleanup(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_memory_serialize_deserialize_roundtrip() {
        let mut memories = HashMap::new();
        let block = MemoryBlock {
            id: "m1".to_string(),
            block_type: "fact".to_string(),
            content: "Paris is the capital of France".to_string(),
            importance: 80,
            created_at: 1000,
            last_accessed: 2000,
            tags: vec!["geography".to_string(), "facts".to_string()],
        };
        memories.insert("m1".to_string(), block);

        let block2 = MemoryBlock {
            id: "m2".to_string(),
            block_type: "preference".to_string(),
            content: "User prefers dark mode".to_string(),
            importance: 50,
            created_at: 3000,
            last_accessed: 4000,
            tags: Vec::new(),
        };
        memories.insert("m2".to_string(), block2);

        let serialized = MemoryStore::serialize_memories(&memories);
        assert!(serialized.contains("\u{00A7}"));
        assert!(serialized.contains("Paris is the capital of France"));
        assert!(serialized.contains("User prefers dark mode"));

        let deserialized = MemoryStore::deserialize_memories(&serialized);
        assert_eq!(deserialized.len(), 2);

        let m1 = deserialized.get("m1").unwrap();
        assert_eq!(m1.block_type, "fact");
        assert_eq!(m1.importance, 80);
        assert_eq!(m1.content, "Paris is the capital of France");
        assert_eq!(m1.tags, vec!["geography", "facts"]);
        assert_eq!(m1.created_at, 1000);
        assert_eq!(m1.last_accessed, 2000);

        let m2 = deserialized.get("m2").unwrap();
        assert_eq!(m2.block_type, "preference");
        assert_eq!(m2.importance, 50);
        assert!(m2.tags.is_empty());
    }

    #[test]
    fn test_profile_serialize_deserialize_roundtrip() {
        let mut profiles = HashMap::new();
        let mut prefs = HashMap::new();
        prefs.insert("theme".to_string(), "dark".to_string());
        prefs.insert("language".to_string(), "en".to_string());

        let fact = MemoryBlock {
            id: "user1_0".to_string(),
            block_type: "fact".to_string(),
            content: "Likes coding".to_string(),
            importance: 50,
            created_at: 0,
            last_accessed: 0,
            tags: Vec::new(),
        };

        let profile = UserProfile {
            user_id: "user1".to_string(),
            name: Some("John Doe".to_string()),
            preferences: prefs,
            facts: vec![fact],
        };
        profiles.insert("user1".to_string(), profile);

        let serialized = MemoryStore::serialize_profiles(&profiles);
        assert!(serialized.contains("\u{00A7}"));
        assert!(serialized.contains("user_id: user1"));
        assert!(serialized.contains("name: John Doe"));
        assert!(serialized.contains("theme: dark"));

        let deserialized = MemoryStore::deserialize_profiles(&serialized);
        assert_eq!(deserialized.len(), 1);

        let p = deserialized.get("user1").unwrap();
        assert_eq!(p.name, Some("John Doe".to_string()));
        assert_eq!(p.preferences.get("theme").unwrap(), "dark");
        assert_eq!(p.preferences.get("language").unwrap(), "en");
        assert_eq!(p.facts.len(), 1);
        assert_eq!(p.facts[0].content, "Likes coding");
    }

    #[test]
    fn test_memory_store_write_read_files() {
        let dir = test_dir("write_read");
        let store = MemoryStore::new(dir.clone());

        let mut memories = HashMap::new();
        memories.insert(
            "k1".to_string(),
            MemoryBlock {
                id: "k1".to_string(),
                block_type: "fact".to_string(),
                content: "Hello world".to_string(),
                importance: 90,
                created_at: 100,
                last_accessed: 200,
                tags: vec!["greeting".to_string()],
            },
        );

        store.write_memories(&memories).unwrap();
        assert!(store.memory_path().exists());

        let loaded = store.read_memories().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get("k1").unwrap().content, "Hello world");

        let mut profiles = HashMap::new();
        profiles.insert(
            "u1".to_string(),
            UserProfile {
                user_id: "u1".to_string(),
                name: Some("Alice".to_string()),
                preferences: HashMap::new(),
                facts: Vec::new(),
            },
        );

        store.write_profiles(&profiles).unwrap();
        assert!(store.user_path().exists());

        let loaded_profiles = store.read_profiles().unwrap();
        assert_eq!(loaded_profiles.len(), 1);
        assert_eq!(
            loaded_profiles.get("u1").unwrap().name,
            Some("Alice".to_string())
        );

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_with_storage_dir_save_load() {
        let dir = test_dir("save_load");
        let manager = MemoryManager::with_storage_dir(dir.clone());

        let block = MemoryBlock {
            id: "persist1".to_string(),
            block_type: "fact".to_string(),
            content: "Rust is great".to_string(),
            importance: 75,
            created_at: 500,
            last_accessed: 600,
            tags: vec!["lang".to_string()],
        };
        manager
            .long_term
            .write()
            .await
            .insert("persist1".to_string(), block);

        let profile = UserProfile {
            user_id: "bob".to_string(),
            name: Some("Bob".to_string()),
            preferences: {
                let mut m = HashMap::new();
                m.insert("editor".to_string(), "vim".to_string());
                m
            },
            facts: Vec::new(),
        };
        manager
            .profiles
            .write()
            .await
            .insert("bob".to_string(), profile);

        manager.save_to_disk().await.unwrap();

        // Create a fresh manager pointing at the same directory and load
        let manager2 = MemoryManager::with_storage_dir(dir.clone());
        manager2.load_from_disk().await.unwrap();

        let loaded_block = manager2.get("persist1").await;
        assert!(loaded_block.is_some());
        assert_eq!(loaded_block.unwrap().content, "Rust is great");

        let loaded_profile = manager2.get_profile("bob").await;
        assert!(loaded_profile.is_some());
        assert_eq!(
            loaded_profile.as_ref().unwrap().name,
            Some("Bob".to_string())
        );
        assert_eq!(
            loaded_profile.unwrap().preferences.get("editor").unwrap(),
            "vim"
        );

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_auto_save_on_store() {
        let dir = test_dir("auto_save_store");
        let manager = MemoryManager::with_storage_dir(dir.clone());

        // store() should auto-save to disk
        let block = MemoryBlock::new("auto1", "fact", "Auto-saved fact")
            .importance(60)
            .tags(vec!["auto".to_string()]);
        manager.store(block).await;

        // Verify the file was written
        let store = MemoryStore::new(dir.clone());
        assert!(store.memory_path().exists());

        let loaded = store.read_memories().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get("auto1").unwrap().content, "Auto-saved fact");

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_auto_save_on_save_profile() {
        let dir = test_dir("auto_save_profile");
        let manager = MemoryManager::with_storage_dir(dir.clone());

        // save_profile() should auto-save to disk
        let profile = UserProfile {
            user_id: "autouser".to_string(),
            name: Some("Auto User".to_string()),
            preferences: HashMap::new(),
            facts: Vec::new(),
        };
        manager.save_profile(profile).await;

        // Verify the file was written
        let store = MemoryStore::new(dir.clone());
        assert!(store.user_path().exists());

        let loaded = store.read_profiles().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.get("autouser").unwrap().name,
            Some("Auto User".to_string())
        );

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_load_from_disk_missing_files() {
        let dir = test_dir("missing_files");
        std::fs::create_dir_all(&dir).unwrap();

        let manager = MemoryManager::with_storage_dir(dir.clone());
        // Should succeed even when MEMORY.md and USER.md don't exist
        manager.load_from_disk().await.unwrap();
        assert!(manager.long_term.read().await.is_empty());
        assert!(manager.profiles.read().await.is_empty());

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_save_to_disk_no_op_without_storage_dir() {
        let manager = MemoryManager::new();
        // Should return Ok(()) without doing anything
        manager.save_to_disk().await.unwrap();
        manager.load_from_disk().await.unwrap();
    }

    #[test]
    fn test_deserialize_empty_content() {
        let memories = MemoryStore::deserialize_memories("");
        assert!(memories.is_empty());

        let profiles = MemoryStore::deserialize_profiles("");
        assert!(profiles.is_empty());
    }

    #[test]
    fn test_memory_md_format_matches_python_pattern() {
        // Verify the serialized format uses § delimiters as specified
        let mut memories = HashMap::new();
        memories.insert(
            "x".to_string(),
            MemoryBlock {
                id: "x".to_string(),
                block_type: "fact".to_string(),
                content: "test content".to_string(),
                importance: 80,
                created_at: 0,
                last_accessed: 0,
                tags: vec!["geography".to_string(), "facts".to_string()],
            },
        );
        let output = MemoryStore::serialize_memories(&memories);
        // Must start with § and contain the expected header format
        assert!(output.starts_with("\u{00A7} ["));
        assert!(output.contains("type: fact"));
        assert!(output.contains("importance: 80"));
        assert!(output.contains("tags: geography,facts"));
        assert!(output.contains("test content"));
    }
}
