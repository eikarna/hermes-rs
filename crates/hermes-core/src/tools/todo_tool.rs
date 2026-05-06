//! Todo list tool
//!
//! In-memory task list management tool matching Python's todo_tool.py.
//! Stores todos per session using a global lazy_static HashMap.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolResult};

use lazy_static::lazy_static;
use std::collections::HashMap;
use std::sync::Mutex;

lazy_static! {
    static ref TODO_STORE: Mutex<HashMap<String, Vec<TodoItem>>> = Mutex::new(HashMap::new());
}

/// A single todo item
#[derive(Debug, Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    /// Unique identifier for this todo
    pub id: String,
    /// Content/description of the task
    pub content: String,
    /// Status: "pending", "in_progress", "completed", or "cancelled"
    pub status: String,
}

impl TodoItem {
    /// Validate that the status is one of the allowed values
    fn is_valid_status(status: &str) -> bool {
        matches!(
            status,
            "pending" | "in_progress" | "completed" | "cancelled"
        )
    }
}

/// Arguments for the todo tool
#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TodoArgs {
    /// The complete list of todos (replaces existing list)
    todos: Vec<TodoItem>,
    /// Session ID to scope the todo list (defaults to "default")
    session_id: Option<String>,
}

/// Tool for managing an in-memory todo list
pub struct TodoTool;

#[async_trait]
impl HermesTool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Manage a task list. Each call replaces the entire todo list for the given session. \
        Supported statuses: pending, in_progress, completed, cancelled."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<TodoArgs>("todo", "Manage task list")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: TodoArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("todo", format!("Invalid arguments: {}", e)),
        };

        let session_id = args.session_id.unwrap_or_else(|| "default".to_string());

        // Validate all statuses
        for item in &args.todos {
            if !TodoItem::is_valid_status(&item.status) {
                return ToolResult::error(
                    "todo",
                    format!(
                        "Invalid status '{}' for todo '{}'. Must be one of: pending, in_progress, completed, cancelled",
                        item.status, item.id
                    ),
                );
            }
        }

        let todos = args.todos;
        let count = todos.len();

        let summary: Vec<Value> = todos
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "content": t.content,
                    "status": t.status,
                })
            })
            .collect();

        // Replace the entire list for this session
        {
            let mut store = match TODO_STORE.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    return ToolResult::error(
                        "todo",
                        format!("Failed to acquire lock on todo store: {}", e),
                    )
                }
            };
            store.insert(session_id.clone(), todos);
        }

        ToolResult::success(
            "todo",
            serde_json::json!({
                "sessionId": session_id,
                "count": count,
                "todos": summary,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    fn default_context() -> ToolContext {
        ToolContext::default()
    }

    #[tokio::test]
    async fn test_todo_replace_list() {
        let tool = TodoTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        { "id": "1", "content": "Write tests", "status": "pending" },
                        { "id": "2", "content": "Review PR", "status": "in_progress" },
                    ],
                    "sessionId": "test-session-1"
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["count"], 2);
        assert_eq!(parsed["sessionId"], "test-session-1");
    }

    #[tokio::test]
    async fn test_todo_default_session() {
        let tool = TodoTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        { "id": "a", "content": "Task A", "status": "completed" }
                    ]
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["sessionId"], "default");
    }

    #[tokio::test]
    async fn test_todo_invalid_status() {
        let tool = TodoTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        { "id": "1", "content": "Bad status", "status": "done" }
                    ]
                }),
                default_context(),
            )
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid status"));
    }

    #[tokio::test]
    async fn test_todo_empty_list() {
        let tool = TodoTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [],
                    "sessionId": "test-empty"
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["count"], 0);
    }

    #[tokio::test]
    async fn test_todo_replaces_previous() {
        let tool = TodoTool;
        let session = "test-replace";

        // First call
        tool.execute(
            serde_json::json!({
                "todos": [
                    { "id": "1", "content": "First", "status": "pending" },
                    { "id": "2", "content": "Second", "status": "pending" },
                ],
                "sessionId": session
            }),
            default_context(),
        )
        .await;

        // Second call replaces entirely
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        { "id": "3", "content": "Third", "status": "completed" }
                    ],
                    "sessionId": session
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["todos"][0]["id"], "3");
    }
}
