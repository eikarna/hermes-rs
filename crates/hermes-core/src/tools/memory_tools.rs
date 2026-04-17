//! Memory operation tools
//!
//! Tools for storing, searching, and recalling memories.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolResult};

// Global memory storage for the memory tools
// In production, this would be backed by a proper database
lazy_static::lazy_static! {
    static ref MEMORY_STORE: Arc<RwLock<HashMap<String, MemoryEntry>>> = Arc::new(RwLock::new(HashMap::new()));
}

#[derive(Debug, Clone)]
struct MemoryEntry {
    content: String,
    block_type: String,
    importance: u8,
    tags: Vec<String>,
    created_at: i64,
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
}

#[async_trait]
impl HermesTool for MemoryStoreTool {
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

        let entry = MemoryEntry {
            content: args.content.clone(),
            block_type: args.block_type.unwrap_or_else(|| "general".to_string()),
            importance: args.importance.unwrap_or(50).min(100),
            tags: args.tags.unwrap_or_default(),
            created_at: now,
        };

        MEMORY_STORE.write().await.insert(args.key.clone(), entry);

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
impl HermesTool for MemorySearchTool {
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
impl HermesTool for MemoryRecallTool {
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
