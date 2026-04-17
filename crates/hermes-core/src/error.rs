//! Error types for hermes-core library
//!
//! Uses `thiserror` for domain-specific errors with rich context.

use thiserror::Error;

/// Result type alias for hermes-core operations
pub type Result<T> = std::result::Result<T, Error>;

/// Domain-specific errors for Hermes-RS
#[derive(Error, Debug)]
pub enum Error {
    // ========== Client Errors ==========
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Failed to parse response: {0}")]
    ParseResponse(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Missing API key")]
    MissingApiKey,

    // ========== Streaming Errors ==========
    #[error("SSE parse error at position {position}: {message}")]
    SseParse { position: usize, message: String },

    #[error("Unexpected SSE event type: {0}")]
    UnexpectedSseEvent(String),

    #[error("Incomplete SSE message")]
    IncompleteSseMessage,

    // ========== Tool Errors ==========
    #[error("Tool not found: {name}")]
    ToolNotFound { name: String },

    #[error("Tool execution failed: {name} - {source}")]
    ToolExecution {
        name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Tool timeout: {name} (exceeded {timeout:?})")]
    ToolTimeout {
        name: String,
        timeout: std::time::Duration,
    },

    #[error("Invalid tool arguments for {name}: {details}")]
    InvalidToolArgs { name: String, details: String },

    #[error("Tool cancelled: {name}")]
    ToolCancelled { name: String },

    // ========== Parser Errors ==========
    #[error("XML parse error: {0}")]
    XmlParse(String),

    #[error("Incomplete XML: {context}")]
    IncompleteXml { context: String },

    #[error("JSON decode error: {0}")]
    JsonDecode(#[from] serde_json::Error),

    // ========== Agent Errors ==========
    #[error("Agent error: {0}")]
    Agent(String),

    #[error("Max iterations exceeded: {max}")]
    MaxIterationsExceeded { max: usize },

    #[error("Context length exceeded")]
    ContextLengthExceeded,

    #[error("Invalid state transition: {from} -> {to}")]
    InvalidStateTransition { from: String, to: String },

    // ========== Schema Errors ==========
    #[error("Schema generation error: {0}")]
    SchemaGeneration(String),

    #[error("Invalid schema: {0}")]
    InvalidSchema(String),

    // ========== Configuration Errors ==========
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Missing required configuration: {key}")]
    MissingConfig { key: String },
}

impl Error {
    /// Returns whether this error indicates a transient failure that might succeed on retry
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Error::Network(_)
                | Error::IncompleteSseMessage
                | Error::ToolTimeout { .. }
                | Error::IncompleteXml { .. }
        )
    }

    /// Returns whether this error should trigger self-healing (re-prompt the LLM)
    pub fn is_self_healing(&self) -> bool {
        matches!(
            self,
            Error::ToolNotFound { .. }
                | Error::InvalidToolArgs { .. }
                | Error::ToolExecution { .. }
                | Error::XmlParse(_)
                | Error::Agent(_)
        )
    }

    /// Get a user-friendly error message for display
    pub fn user_message(&self) -> String {
        match self {
            Error::ToolNotFound { name } => {
                format!("The requested tool '{}' is not available.", name)
            }
            Error::ToolExecution { name, .. } => {
                format!("Tool '{}' encountered an error during execution.", name)
            }
            Error::ToolTimeout { name, .. } => {
                format!("Tool '{}' timed out.", name)
            }
            Error::InvalidToolArgs { name, details } => {
                format!("Invalid arguments for tool '{}': {}", name, details)
            }
            Error::MaxIterationsExceeded { max } => {
                format!("Maximum iterations ({}) exceeded.", max)
            }
            Error::ContextLengthExceeded => {
                "The conversation has exceeded the maximum context length.".to_string()
            }
            _ => self.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        let tool_not_found = Error::ToolNotFound {
            name: "test_tool".to_string(),
        };
        assert!(tool_not_found.is_self_healing());
        assert!(!tool_not_found.is_transient());
    }
}
