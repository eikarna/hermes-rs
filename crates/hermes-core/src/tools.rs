//! Tool system for Hermes-RS
//!
//! This module provides the core tool infrastructure including:
//! - `HermesTool` trait for defining tools
//! - `ToolRegistry` for managing and executing tools
//! - Built-in tools for common operations

pub mod builtin;
pub mod file_tools;
pub mod terminal_tool;
pub mod web_tools;
pub mod code_execution;
pub mod memory_tools;
pub mod http_tool;
pub mod datetime_tool;
pub mod todo_tool;
pub mod clarify_tool;
pub mod patch_tool;

// Re-export commonly used types
pub use builtin::{
    register_builtin_tools, builtin_tool_names,
    FileReadTool, FileWriteTool, FileSearchTool, FileListTool,
    TerminalTool, WebSearchTool, WebFetchTool, CodeExecutionTool,
    MemoryStoreTool, MemorySearchTool, MemoryRecallTool,
    HttpRequestTool, DateTimeTool, TimestampTool,
    TodoTool, ClarifyTool, PatchTool,
};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, instrument, warn};

use crate::error::{Error, Result};
use crate::schema::ToolSchema;

/// Result of tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Tool call ID this result is for
    pub tool_call_id: String,
    /// Whether the execution succeeded
    pub success: bool,
    /// Result content (serialized JSON or error message)
    pub content: String,
    /// Optional error details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolResult {
    /// Create a successful result
    pub fn success<T: Serialize>(tool_call_id: impl Into<String>, content: T) -> Self {
        let content = serde_json::to_string(&content).unwrap_or_else(|_| "{}".to_string());
        Self {
            tool_call_id: tool_call_id.into(),
            success: true,
            content,
            error: None,
        }
    }

    /// Create an error result
    pub fn error(tool_call_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            success: false,
            content: String::new(),
            error: Some(error.into()),
        }
    }

    /// Create a result from a serde_json::Value
    pub fn from_value(tool_call_id: impl Into<String>, value: Value) -> Self {
        let content = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
        Self {
            tool_call_id: tool_call_id.into(),
            success: true,
            content,
            error: None,
        }
    }

    /// Get the content as a parsed JSON value
    pub fn parse_content<T: for<'de> Deserialize<'de>>(&self) -> Result<T> {
        serde_json::from_str(&self.content)
            .map_err(|e| Error::ParseResponse(format!("Failed to parse tool result: {}", e)))
    }
}

/// Trait for defining a Hermes tool
///
/// Tools must provide:
/// - A unique name
/// - A description for the LLM
/// - A JSON Schema for their parameters
/// - An async execute method
#[async_trait]
pub trait HermesTool: Send + Sync {
    /// Get the tool's unique name
    fn name(&self) -> &str;

    /// Get the tool's description
    fn description(&self) -> &str;

    /// Get the JSON Schema for the tool's parameters
    fn schema(&self) -> ToolSchema;

    /// Execute the tool with the given arguments
    ///
    /// # Arguments
    /// * `args` - JSON object containing the tool arguments
    /// * `context` - Additional execution context
    ///
    /// # Returns
    /// A `ToolResult` containing the execution outcome
    async fn execute(&self, args: Value, context: ToolContext) -> ToolResult;
}

/// Context passed to tool execution
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Additional metadata about the execution
    pub metadata: HashMap<String, String>,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            metadata: HashMap::new(),
        }
    }
}

impl ToolContext {
    /// Create a new context with metadata
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Get a metadata value
    pub fn get(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(|s| s.as_str())
    }
}

/// Internal message for tool execution
#[derive(Debug)]
#[allow(dead_code)]
enum ToolCommand {
    Execute {
        tool_name: String,
        tool_call_id: String,
        args: Value,
        response_tx: oneshot::Sender<ToolResult>,
    },
    Shutdown,
}

/// Sandboxed tool executor with timeout support
struct ToolExecutor {
    timeout: Duration,
}

impl ToolExecutor {
    fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    async fn execute_with_timeout(
        &self,
        tool: Arc<dyn HermesTool>,
        tool_name: String,
        tool_call_id: String,
        args: Value,
        context: ToolContext,
    ) -> ToolResult {
        let result = timeout(
            self.timeout,
            tool.execute(args, context),
        ).await;

        match result {
            Ok(result) => result,
            Err(_) => {
                warn!(tool = %tool_name, timeout = ?self.timeout, "Tool execution timed out");
                ToolResult::error(tool_call_id, format!("Tool timed out after {:?}", self.timeout))
            }
        }
    }
}

/// Registry for managing and executing tools
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Arc<dyn HermesTool>>>>,
    executor: ToolExecutor,
    #[allow(dead_code)]
    command_tx: mpsc::Sender<ToolCommand>,
}

impl ToolRegistry {
    /// Create a new tool registry
    pub fn new(timeout: Duration) -> Self {
        let tools = Arc::new(RwLock::new(HashMap::new()));
        let (command_tx, command_rx) = mpsc::channel(100);

        let registry = Self {
            tools,
            executor: ToolExecutor::new(timeout),
            command_tx,
        };

        // Start the background worker
        let tools_clone = registry.tools.clone();
        let executor = ToolExecutor::new(timeout);
        tokio::spawn(async move {
            registry_worker(tools_clone, executor, command_rx).await;
        });

        registry
    }

    /// Register a tool
    #[instrument(skip(self, tool), fields(tool = % tool.name()))]
    pub async fn register<T: HermesTool + 'static>(&self, tool: T) -> Result<()> {
        let name = tool.name().to_string();
        let mut tools = self.tools.write().await;

        if tools.contains_key(&name) {
            warn!(tool = %name, "Tool already registered, replacing");
        }

        tools.insert(name.clone(), Arc::new(tool));
        info!(tool = %name, "Tool registered successfully");
        Ok(())
    }

    /// Get all registered tool schemas
    pub async fn get_schemas(&self) -> Vec<ToolSchema> {
        let tools = self.tools.read().await;
        tools.values().map(|t| t.schema()).collect()
    }

    /// Get a tool by name
    pub async fn get(&self, name: &str) -> Option<Arc<dyn HermesTool>> {
        let tools = self.tools.read().await;
        tools.get(name).cloned()
    }

    /// Check if a tool is registered
    pub async fn contains(&self, name: &str) -> bool {
        let tools = self.tools.read().await;
        tools.contains_key(name)
    }

    /// Get the number of registered tools
    pub async fn len(&self) -> usize {
        let tools = self.tools.read().await;
        tools.len()
    }

    /// Check if no tools are registered
    pub async fn is_empty(&self) -> bool {
        let tools = self.tools.read().await;
        tools.is_empty()
    }

    /// Execute a tool by name with arguments
    #[instrument(skip(self, args, context), fields(tool = % tool_name))]
    pub async fn execute(
        &self,
        tool_name: &str,
        tool_call_id: &str,
        args: Value,
        context: ToolContext,
    ) -> Result<ToolResult> {
        let tool = {
            let tools = self.tools.read().await;
            tools.get(tool_name).cloned()
        };

        match tool {
            Some(tool) => {
                let name = tool_name.to_string();
                let id = tool_call_id.to_string();
                debug!(tool = %name, args = ?args, "Executing tool");
                let result = self.executor
                    .execute_with_timeout(tool, name, id, args, context)
                    .await;
                Ok(result)
            }
            None => {
                error!(tool = %tool_name, "Tool not found in registry");
                Err(Error::ToolNotFound { name: tool_name.to_string() })
            }
        }
    }

    /// Execute multiple tools concurrently
    #[allow(dead_code)]
    pub async fn execute_all(
        &self,
        requests: Vec<(String, String, Value, ToolContext)>,
    ) -> Vec<Result<ToolResult>> {
        let mut results = Vec::new();
        for (name, id, args, ctx) in requests {
            results.push(self.execute(&name, &id, args, ctx).await);
        }
        results
    }
}

/// Background worker for tool execution
async fn registry_worker(
    tools: Arc<RwLock<HashMap<String, Arc<dyn HermesTool>>>>,
    executor: ToolExecutor,
    mut command_rx: mpsc::Receiver<ToolCommand>,
) {
    info!("Tool registry worker started");

    while let Some(cmd) = command_rx.recv().await {
        match cmd {
            ToolCommand::Execute { tool_name, tool_call_id, args, response_tx } => {
                let tool = {
                    let tools = tools.read().await;
                    tools.get(&tool_name).cloned()
                };

                let result = match tool {
                    Some(tool) => {
                        executor
                            .execute_with_timeout(
                                tool,
                                tool_name,
                                tool_call_id.clone(),
                                args,
                                ToolContext::default(),
                            )
                            .await
                    }
                    None => ToolResult::error(&tool_call_id, format!("Tool '{}' not found", tool_name)),
                };

                let _ = response_tx.send(result);
            }
            ToolCommand::Shutdown => {
                info!("Tool registry worker shutting down");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;

    #[derive(JsonSchema, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestArgs {
        query: String,
        limit: Option<i32>,
    }

    struct TestTool;

    #[async_trait]
    impl HermesTool for TestTool {
        fn name(&self) -> &str {
            "test_tool"
        }

        fn description(&self) -> &str {
            "A test tool for unit testing"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema::from_type::<TestArgs>("test_tool", "A test tool")
        }

        async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
            if let Some(query) = args.get("query").and_then(|v| v.as_str()) {
                ToolResult::success("call_1", serde_json::json!({ "result": format!("Processed: {}", query) }))
            } else {
                ToolResult::error("call_1", "Missing 'query' argument")
            }
        }
    }

    #[tokio::test]
    async fn test_registry_operations() {
        let registry = ToolRegistry::new(Duration::from_secs(5));

        // Register a tool
        registry.register(TestTool).await.unwrap();

        // Check tool exists
        assert!(registry.contains("test_tool").await);
        assert_eq!(registry.len().await, 1);

        // Get schemas
        let schemas = registry.get_schemas().await;
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "test_tool");
    }

    #[tokio::test]
    async fn test_tool_execution() {
        let registry = ToolRegistry::new(Duration::from_secs(5));
        registry.register(TestTool).await.unwrap();

        let args = serde_json::json!({
            "query": "test query",
            "limit": 10
        });

        let result = registry
            .execute("test_tool", "call_1", args, ToolContext::default())
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.content.contains("Processed:"));
    }

    #[tokio::test]
    async fn test_tool_not_found() {
        let registry = ToolRegistry::new(Duration::from_secs(5));

        let result = registry
            .execute("nonexistent", "call_1", serde_json::json!({}), ToolContext::default())
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::ToolNotFound { name } => assert_eq!(name, "nonexistent"),
            _ => panic!("Expected ToolNotFound error"),
        }
    }
}