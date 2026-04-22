//! Built-in tools for Hermes-RS
//!
//! This module aggregates all built-in tools and provides a convenient
//! function to register them all with a ToolRegistry.

use crate::client::OpenAIClient;
use crate::error::Result;
use crate::tools::ToolRegistry;

pub use super::clarify_tool::ClarifyTool;
pub use super::code_execution::CodeExecutionTool;
pub use super::datetime_tool::{DateTimeTool, TimestampTool};
pub use super::file_tools::{FileListTool, FileReadTool, FileSearchTool, FileWriteTool};
pub use super::http_tool::HttpRequestTool;
pub use super::memory_tools::{MemoryRecallTool, MemorySearchTool, MemoryStoreTool};
pub use super::patch_tool::PatchTool;
pub use super::sub_agent_tool::SubAgentTool;
pub use super::terminal_tool::TerminalTool;
pub use super::todo_tool::TodoTool;
pub use super::web_tools::{WebFetchTool, WebSearchTool};

/// Register all built-in tools with a registry
pub async fn register_builtin_tools(registry: &ToolRegistry) -> Result<()> {
    registry.register(FileReadTool).await?;
    registry.register(FileWriteTool).await?;
    registry.register(FileSearchTool).await?;
    registry.register(FileListTool).await?;
    registry.register(TerminalTool).await?;
    registry.register(WebSearchTool).await?;
    registry.register(WebFetchTool).await?;
    registry.register(CodeExecutionTool).await?;
    registry.register(MemoryStoreTool).await?;
    registry.register(MemorySearchTool).await?;
    registry.register(MemoryRecallTool).await?;
    registry.register(HttpRequestTool).await?;
    registry.register(DateTimeTool).await?;
    registry.register(TimestampTool).await?;
    registry.register(TodoTool).await?;
    registry.register(ClarifyTool).await?;
    registry.register(PatchTool).await?;

    Ok(())
}

/// Register all built-in tools plus the sub-agent delegation tool.
pub async fn register_builtin_tools_with_sub_agent(
    registry: &ToolRegistry,
    parent_client: &OpenAIClient,
    model: impl Into<String>,
) -> Result<()> {
    register_builtin_tools(registry).await?;
    registry
        .register(SubAgentTool::new(parent_client, model.into()))
        .await?;
    Ok(())
}

/// Get a list of all built-in tool names
pub fn builtin_tool_names() -> Vec<&'static str> {
    vec![
        "file_read",
        "file_write",
        "file_search",
        "file_list",
        "terminal",
        "web_search",
        "web_fetch",
        "code_execution",
        "memory_store",
        "memory_search",
        "memory_recall",
        "http_request",
        "datetime",
        "timestamp",
        "todo",
        "clarify",
        "patch",
        "delegate_to_sub_agent",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_register_all_builtin_tools() {
        let registry = ToolRegistry::new(Duration::from_secs(5));
        register_builtin_tools(&registry).await.unwrap();

        let schemas = registry.get_schemas().await;
        assert_eq!(schemas.len() + 1, builtin_tool_names().len());
        assert!(!registry.contains("delegate_to_sub_agent").await);
    }

    #[tokio::test]
    async fn test_register_builtin_tools_with_sub_agent() {
        let registry = ToolRegistry::new(Duration::from_secs(5));
        let client = OpenAIClient::new(crate::client::ClientConfig::default());

        register_builtin_tools_with_sub_agent(&registry, &client, "gpt-4.1")
            .await
            .unwrap();

        let schemas = registry.get_schemas().await;
        assert_eq!(schemas.len(), builtin_tool_names().len());
        assert!(registry.contains("delegate_to_sub_agent").await);
    }
}
