//! Context management with automatic compression
//!
//! When conversation history exceeds the model's context window,
//! this module automatically compresses older messages while preserving
//! the most important context.

use crate::client::{Message, Role};
use crate::error::Result;
use std::collections::VecDeque;

/// Configuration for context management
#[derive(Debug, Clone, Copy)]
pub struct ContextConfig {
    /// Maximum context length in tokens
    pub max_context_length: usize,
    /// Reserved space for response (to ensure we have room for reply)
    pub response_buffer: usize,
    /// Minimum messages to preserve when compressing
    pub min_messages_preserve: usize,
    /// Compression ratio when truncating
    pub compression_ratio: f32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_context_length: 128_000,
            response_buffer: 4000,
            min_messages_preserve: 4,
            compression_ratio: 0.5,
        }
    }
}

/// A message with token count for tracking
#[derive(Debug, Clone)]
struct MessageWithTokens {
    message: Message,
    tokens: usize,
}

/// Context manager for handling long conversations
#[derive(Debug)]
pub struct ContextManager {
    config: ContextConfig,
    history: VecDeque<MessageWithTokens>,
    total_tokens: usize,
}

impl ContextManager {
    /// Create a new context manager
    pub fn new(config: ContextConfig) -> Self {
        Self {
            config,
            history: VecDeque::new(),
            total_tokens: 0,
        }
    }

    /// Add a message to the context
    pub fn add_message(&mut self, message: Message) {
        let tokens = estimate_tokens(&message.content);
        self.total_tokens += tokens;
        self.history
            .push_back(MessageWithTokens { message, tokens });
    }

    /// Add a message without counting tokens (for pre-counted messages)
    pub fn push_message(&mut self, message: Message, tokens: usize) {
        self.total_tokens += tokens;
        self.history
            .push_back(MessageWithTokens { message, tokens });
    }

    /// Get messages that fit within the context window
    pub fn get_messages(&self) -> Vec<Message> {
        let available_tokens = self
            .config
            .max_context_length
            .saturating_sub(self.config.response_buffer);

        let mut result = Vec::new();
        let mut used_tokens = 0;

        for msg_with_tokens in self.history.iter().rev() {
            if used_tokens + msg_with_tokens.tokens > available_tokens {
                break;
            }
            used_tokens += msg_with_tokens.tokens;
            result.push(msg_with_tokens.message.clone());
        }

        result.reverse();
        result
    }

    /// Get all messages without truncation
    pub fn get_all_messages(&self) -> Vec<Message> {
        self.history.iter().map(|m| m.message.clone()).collect()
    }

    /// Get current token count
    pub fn token_count(&self) -> usize {
        self.total_tokens
    }

    /// Check if context needs compression
    pub fn needs_compression(&self) -> bool {
        let available_tokens = self
            .config
            .max_context_length
            .saturating_sub(self.config.response_buffer);
        self.total_tokens > available_tokens
    }

    /// Compress the context by removing older messages
    pub fn compress(&mut self) {
        let available_tokens = self
            .config
            .max_context_length
            .saturating_sub(self.config.response_buffer);

        // Keep at least min_messages_preserve
        while self.total_tokens > available_tokens
            && self.history.len() > self.config.min_messages_preserve
        {
            if let Some(front) = self.history.pop_front() {
                self.total_tokens = self.total_tokens.saturating_sub(front.tokens);
            } else {
                break;
            }
        }

        // If still too large, aggressive compression
        if self.total_tokens > available_tokens {
            let preserve_count =
                (self.history.len() as f32 * self.config.compression_ratio) as usize;
            let preserve_count = preserve_count.max(self.config.min_messages_preserve);

            while self.history.len() > preserve_count {
                if let Some(front) = self.history.pop_front() {
                    self.total_tokens = self.total_tokens.saturating_sub(front.tokens);
                } else {
                    break;
                }
            }
        }
    }

    /// Clear all context
    pub fn clear(&mut self) {
        self.history.clear();
        self.total_tokens = 0;
    }

    /// Get the number of messages in context
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// Check if context is empty
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// Build a compressed context, removing oldest messages until it fits
    pub fn build_context(&mut self) -> Vec<Message> {
        if self.needs_compression() {
            self.compress();
        }
        self.get_messages()
    }
}

/// Estimate token count for a string (rough approximation)
///
/// This uses a simple heuristic: ~4 characters per token on average.
/// For more accurate counting, a proper tokenizer like tiktoken would be needed.
pub fn estimate_tokens(text: &str) -> usize {
    // Rough estimate: ~4 characters per token for English text
    // This is a simplification but works reasonably well
    (text.len() as f32 / 4.0).ceil() as usize
}

/// Estimate tokens in a message
pub fn estimate_message_tokens(message: &Message) -> usize {
    let content_tokens = estimate_tokens(&message.content);

    // Add overhead for role and other fields
    let role_overhead = 4; // tokens for role indicator
    let tool_call_overhead = if message.tool_calls.is_some() { 10 } else { 0 };

    content_tokens + role_overhead + tool_call_overhead
}

/// Compress conversation history by summarizing or truncating
pub fn compress_conversation(messages: &[Message], max_tokens: usize) -> Result<Vec<Message>> {
    let mut result = Vec::new();
    let mut tokens_used = 0;

    // Always keep system prompt if present
    let system_prompt = messages.iter().find(|m| m.role == Role::System);

    for message in messages.iter().rev() {
        let msg_tokens = estimate_message_tokens(message);

        if tokens_used + msg_tokens > max_tokens {
            // If this is the last message that would exceed, stop adding
            break;
        }

        // Skip system prompt (we'll add it separately at the start)
        if message.role == Role::System {
            continue;
        }

        result.push(message.clone());
        tokens_used += msg_tokens;
    }

    // Reverse to get correct order
    result.reverse();

    // Add system prompt at the beginning if we have one
    if let Some(system) = system_prompt {
        result.insert(0, system.clone());
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_estimation() {
        let text = "Hello, world! This is a test message.";
        let tokens = estimate_tokens(text);
        assert!(tokens > 0);
    }

    #[test]
    fn test_context_manager_add() {
        let config = ContextConfig::default();
        let mut ctx = ContextManager::new(config);

        ctx.add_message(Message::user("Hello"));
        assert_eq!(ctx.len(), 1);
        assert!(ctx.token_count() > 0);
    }

    #[test]
    fn test_context_manager_compression() {
        let config = ContextConfig {
            max_context_length: 50,
            response_buffer: 10,
            min_messages_preserve: 2,
            compression_ratio: 0.5,
        };
        let mut ctx = ContextManager::new(config);

        // Add several long messages to force compression
        for i in 0..10 {
            ctx.add_message(Message::user(format!(
                "This is a very long message number {} that should use many tokens when counted",
                i
            )));
        }

        assert!(ctx.len() > config.min_messages_preserve);
        let initial_len = ctx.len();

        ctx.compress();

        // Should have fewer messages after compression
        assert!(ctx.len() < initial_len);
    }

    #[test]
    fn test_compress_conversation() {
        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
            Message::user("How are you?"),
            Message::assistant("I'm doing great!"),
        ];

        let compressed = compress_conversation(&messages, 50).unwrap();

        // Should preserve system prompt and some conversation
        assert!(!compressed.is_empty());
        assert!(
            compressed[0].role == Role::System || compressed.iter().any(|m| m.role == Role::System)
        );
    }
}
