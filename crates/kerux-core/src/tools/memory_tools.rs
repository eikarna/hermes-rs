//! Memory operation tools
//!
//! Tools for storing, searching, and recalling memories.
//! Backed by a JSON file (`~/.kerux/memory/memories.json`) so memories
//! survive restarts; the in-memory map is a cache loaded at first access.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::schema::ToolSchema;
use crate::tools::{KeruxTool, ToolContext, ToolResult};

/// Path of the memory store file.
fn memory_file() -> std::path::PathBuf {
    crate::persist::data_dir("memory").join("memories.json")
}

/// Load the memory map from disk (empty when missing/corrupt).
fn load_from_disk() -> HashMap<String, MemoryEntry> {
    crate::persist::read_json::<HashMap<String, MemoryEntry>>(&memory_file()).unwrap_or_default()
}

/// Persist the memory map to disk. Errors are logged, not fatal.
fn save_to_disk(map: &HashMap<String, MemoryEntry>) {
    if let Err(e) = crate::persist::write_json(&memory_file(), map) {
        tracing::warn!(error = %e, "Failed to persist memory store");
    }
}

// Global memory storage for the memory tools, seeded from disk on first
// access so memories survive restarts.
lazy_static::lazy_static! {
    static ref MEMORY_STORE: Arc<RwLock<HashMap<String, MemoryEntry>>> = Arc::new(RwLock::new(load_from_disk()));
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryEntry {
    content: String,
    block_type: String,
    importance: u8,
    tags: Vec<String>,
    created_at: i64,
    #[serde(default)]
    source: String,
    #[serde(default)]
    trust: u8,
}

/// Tool for storing a memory
pub struct MemoryStoreTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryStoreArgs {
    key: String,
    content: String,
    block_type: Option<String>,
    importance: Option<u8>,
    tags: Option<Vec<String>>,
    source: Option<String>,
    trust: Option<u8>,
}

#[async_trait]
impl KeruxTool for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn description(&self) -> &str {
        "Store a piece of information in long-term memory. Useful for remembering facts, preferences, or user information."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<MemoryStoreArgs>("memory_store", "Store information in memory")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: MemoryStoreArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolResult::error("memory_store", format!("Invalid arguments: {}", e))
            }
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let config = crate::config::runtime_config().memory;
        let content = if config.mask_secrets {
            crate::redaction::redact_text(&args.content)
        } else {
            args.content.clone()
        };

        let mem_source: crate::memory::MemorySource = args
            .source
            .as_deref()
            .unwrap_or("tool")
            .parse()
            .unwrap_or(crate::memory::MemorySource::Tool);

        let trust = args.trust.unwrap_or_else(|| mem_source.default_trust());

        let mut tags = args.tags.unwrap_or_default();
        if config.quarantine_low_trust
            && trust < config.trust_threshold
            && !tags.contains(&"quarantined".to_string())
        {
            tags.push("quarantined".to_string());
        }

        let entry = MemoryEntry {
            content,
            block_type: args.block_type.unwrap_or_else(|| "general".to_string()),
            importance: args.importance.unwrap_or(50).min(100),
            tags,
            created_at: now,
            source: mem_source.to_string(),
            trust,
        };

        {
            let mut store = MEMORY_STORE.write().await;
            store.insert(args.key.clone(), entry);
            save_to_disk(&store);
        }

        ToolResult::success(
            "memory_store",
            serde_json::json!({
                "key": args.key,
                "stored": true,
                "timestamp": now
            }),
        )
    }
}

/// Tool for searching memories
pub struct MemorySearchTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemorySearchArgs {
    query: String,
    max_results: Option<usize>,
}

#[async_trait]
impl KeruxTool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search long-term memory for information matching a query. Searches both content and tags."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<MemorySearchArgs>("memory_search", "Search memories")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: MemorySearchArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolResult::error("memory_search", format!("Invalid arguments: {}", e))
            }
        };

        let max_results = args.max_results.unwrap_or(10).min(50);
        let query_lower = args.query.to_lowercase();

        let store = MEMORY_STORE.read().await;
        let mut results = Vec::new();

        for (key, entry) in store.iter() {
            let content_match = entry.content.to_lowercase().contains(&query_lower);
            let tag_match = entry
                .tags
                .iter()
                .any(|t| t.to_lowercase().contains(&query_lower));
            let type_match = entry.block_type.to_lowercase().contains(&query_lower);

            if content_match || tag_match || type_match {
                results.push(serde_json::json!({
                    "key": key,
                    "content": entry.content,
                    "block_type": entry.block_type,
                    "importance": entry.importance,
                    "tags": entry.tags,
                    "created_at": entry.created_at,
                    "relevance": if content_match { 1.0 } else { 0.5 }
                }));

                if results.len() >= max_results {
                    break;
                }
            }
        }

        // Sort by relevance (content match first)
        results.sort_by(|a, b| {
            let relevance_a = a["relevance"].as_f64().unwrap_or(0.0);
            let relevance_b = b["relevance"].as_f64().unwrap_or(0.0);
            relevance_b
                .partial_cmp(&relevance_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        ToolResult::success(
            "memory_search",
            serde_json::json!({
                "query": args.query,
                "results": results,
                "count": results.len()
            }),
        )
    }
}

/// Tool for recalling a specific memory
pub struct MemoryRecallTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryRecallArgs {
    key: String,
}

#[async_trait]
impl KeruxTool for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }

    fn description(&self) -> &str {
        "Recall a specific memory by its key. Use this when you know the exact key of the memory you want to retrieve."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<MemoryRecallArgs>("memory_recall", "Recall a specific memory")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: MemoryRecallArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolResult::error("memory_recall", format!("Invalid arguments: {}", e))
            }
        };

        let store = MEMORY_STORE.read().await;

        match store.get(&args.key) {
            Some(entry) => ToolResult::success(
                "memory_recall",
                serde_json::json!({
                    "key": args.key,
                    "content": entry.content,
                    "block_type": entry.block_type,
                    "importance": entry.importance,
                    "tags": entry.tags,
                    "created_at": entry.created_at,
                    "found": true
                }),
            ),
            None => ToolResult::success(
                "memory_recall",
                serde_json::json!({
                    "key": args.key,
                    "found": false
                }),
            ),
        }
    }
}
