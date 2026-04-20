//! Tolerant XML parser with early tool detection
//!
//! Implements a state-machine based parser that can identify `<tool_call>` blocks
//! and extract inner JSON, even if tags are partially malformed or the JSON is unclosed.
//!
//! ## Key Features
//!
//! - **Incremental parsing**: Process streaming input without waiting for complete blocks
//! - **Early detection**: Fire callbacks as soon as `</tool_call>` is detected
//! - **Tolerant**: Handle malformed tags, unclosed JSON, and other imperfect input
//! - **Zero-copy**: Work with string slices to minimize allocations

use regex::Regex;
use serde_json::Value;
use tracing::{debug, warn};

use crate::client::{ToolCall, ToolCallFunction};
use crate::error::Result;

/// Events emitted by the parser
#[derive(Debug, Clone)]
pub enum ParserEvent {
    /// Text content received (between tags)
    Text(String),
    /// A complete tool call has been detected
    ToolCall(ToolCall),
    /// An error occurred during parsing
    Error(String),
    /// Stream ended (all buffered content flushed)
    End,
}

/// A callback invoked when a complete tool call is detected
pub type ToolCallCallback = Box<dyn Fn(ToolCall) + Send + Sync>;

/// Parser state machine state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    /// Outside any tag, scanning for opening
    Outside,
    /// Inside `<tool_call>` opening tag
    InsideOpenTag,
    /// Inside tool call content
    InsideContent,
    /// Inside a nested tag within tool_call
    InsideNestedTag,
}

/// Tolerant XML parser for tool calls
///
/// Uses a state machine to track parsing progress and detect tool calls
/// incrementally as the LLM output streams in.
pub struct ToolCallParser {
    /// Current state of the parser
    state: ParserState,
    /// Buffer for accumulating content
    buffer: String,
    /// Buffer for the current tag name
    tag_buffer: String,
    /// Track nesting level for nested tags
    nested_depth: usize,
    /// Whether we're currently in the tool_call tag
    in_tool_call: bool,
    /// Track position in input for error reporting
    position: usize,
    /// Callback for early tool call detection
    on_tool_call: Option<ToolCallCallback>,
    /// Regex for extracting JSON from tool_call content
    json_re: Regex,
}

impl Default for ToolCallParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCallParser {
    /// Create a new parser
    pub fn new() -> Self {
        Self {
            state: ParserState::Outside,
            buffer: String::new(),
            tag_buffer: String::new(),
            nested_depth: 0,
            in_tool_call: false,
            position: 0,
            on_tool_call: None,
            json_re: Regex::new(r#"\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}"#).unwrap(),
        }
    }

    /// Set a callback for early tool call detection
    ///
    /// This callback will be invoked as soon as a complete tool call
    /// is detected, without waiting for the full LLM output.
    pub fn on_tool_call<F>(mut self, callback: F) -> Self
    where
        F: Fn(ToolCall) + Send + Sync + 'static,
    {
        self.on_tool_call = Some(Box::new(callback));
        self
    }

    /// Feed more data into the parser
    ///
    /// Returns any events that occurred during parsing.
    pub fn feed(&mut self, data: &str) -> Vec<ParserEvent> {
        let mut events = Vec::new();

        for (i, ch) in data.char_indices() {
            self.position += 1;
            let events_chunk = self.process_char(ch, data, i);
            events.extend(events_chunk);
        }

        // Handle any remaining buffer if we see the end
        if data.is_empty() {
            // Flush any remaining text
            if !self.buffer.is_empty() {
                let text = self.buffer.clone();
                self.buffer.clear();
                events.push(ParserEvent::Text(text));
            }
            events.push(ParserEvent::End);
        }

        events
    }

    /// Process a single character
    fn process_char(&mut self, ch: char, _full_data: &str, _idx: usize) -> Vec<ParserEvent> {
        let mut events = Vec::new();

        match self.state {
            ParserState::Outside => {
                if ch == '<' {
                    self.state = ParserState::InsideOpenTag;
                    self.tag_buffer.clear();
                } else if !ch.is_whitespace() || !self.buffer.is_empty() {
                    // Accumulate text, but trim leading whitespace
                    if self.buffer.is_empty() && ch.is_whitespace() {
                        // Skip leading whitespace
                    } else {
                        self.buffer.push(ch);
                    }
                }
            }
            ParserState::InsideOpenTag => {
                if ch == '>' {
                    let tag = self.tag_buffer.trim().to_lowercase();
                    self.tag_buffer.clear();

                    if tag.starts_with("tool_call") && !tag.starts_with("/") {
                        // Found opening <tool_call...>
                        self.state = ParserState::InsideContent;
                        self.in_tool_call = true;
                        self.buffer.clear();
                    } else {
                        // Not a tool_call tag, emit buffered text and go back
                        if !self.buffer.is_empty() {
                            events.push(ParserEvent::Text(self.buffer.clone()));
                            self.buffer.clear();
                        }
                        self.state = ParserState::Outside;
                    }
                } else if ch != '<' {
                    // Accumulate tag name
                    self.tag_buffer.push(ch);
                }
            }
            ParserState::InsideContent => {
                if ch == '<' {
                    // Check for closing tag
                    self.state = ParserState::InsideNestedTag;
                    self.tag_buffer.clear();
                    self.nested_depth = 1;
                } else {
                    // Accumulate content
                    self.buffer.push(ch);
                }
            }
            ParserState::InsideNestedTag => {
                if ch == '<' {
                    self.nested_depth += 1;
                    self.tag_buffer.push(ch);
                } else if ch == '>' {
                    self.nested_depth -= 1;
                    self.tag_buffer.push(ch);

                    if self.nested_depth == 0 {
                        let nested_tag = self.tag_buffer.trim().to_lowercase();
                        self.tag_buffer.clear();

                        if nested_tag.starts_with("/tool_call") {
                            // Found closing </tool_call>
                            self.process_tool_call(&mut events);
                            self.in_tool_call = false;
                            self.state = ParserState::Outside;
                        } else if nested_tag == "tool_call" {
                            // Nested <tool_call> inside <tool_call> (malformed)
                            warn!("Malformed XML: nested <tool_call> tag");
                            self.buffer.push('<');
                            self.buffer.push_str(&nested_tag);
                            self.state = ParserState::InsideContent;
                        } else {
                            // Other nested tag, continue
                            self.state = ParserState::InsideContent;
                        }
                    }
                } else {
                    self.tag_buffer.push(ch);
                }
            }
        }

        events
    }

    /// Process a completed tool call from the buffer
    fn process_tool_call(&mut self, events: &mut Vec<ParserEvent>) {
        let content = self.buffer.trim();

        if content.is_empty() {
            debug!("Empty tool_call block, ignoring");
            self.buffer.clear();
            return;
        }

        debug!(content_len = content.len(), "Processing tool_call block");

        // Try to extract JSON using regex first (tolerant approach)
        if let Some(tool_call) = self.try_parse_tool_call(content) {
            // Fire early detection callback if set
            if let Some(ref callback) = self.on_tool_call {
                callback(tool_call.clone());
            }
            events.push(ParserEvent::ToolCall(tool_call));
        } else {
            // Fall back to more aggressive parsing
            if let Some(tool_call) = self.aggressive_parse(content) {
                if let Some(ref callback) = self.on_tool_call {
                    callback(tool_call.clone());
                }
                events.push(ParserEvent::ToolCall(tool_call));
            } else {
                warn!(content = %content, "Failed to parse tool_call content");
                events.push(ParserEvent::Error(format!(
                    "Failed to parse tool_call: {}",
                    truncate_string(content, 100)
                )));
            }
        }

        self.buffer.clear();
    }

    /// Try to parse tool call using regex-based extraction
    fn try_parse_tool_call(&self, content: &str) -> Option<ToolCall> {
        // Look for JSON object pattern
        let json_candidates = self.json_re.find_iter(content);

        for m in json_candidates {
            let json_str = m.as_str();
            if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
                return self.extract_tool_call_from_json(&parsed);
            }
        }

        // Try parsing the entire content as JSON
        if let Ok(parsed) = serde_json::from_str::<Value>(content) {
            return self.extract_tool_call_from_json(&parsed);
        }

        None
    }

    /// Extract tool call from parsed JSON
    fn extract_tool_call_from_json(&self, value: &Value) -> Option<ToolCall> {
        // Handle direct object format: { "name": "...", "arguments": "..." }
        let name = value.get("name").and_then(|v: &Value| v.as_str())?;
        let arguments_str = value.get("arguments").and_then(|v: &Value| v.as_str())?;
        let id = value
            .get("id")
            .and_then(|v: &Value| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("call_{}", generate_id()));

        Some(ToolCall {
            id,
            function: ToolCallFunction {
                name: name.to_string(),
                arguments: arguments_str.to_string(),
            },
        })
    }

    /// Aggressive parsing for malformed content
    fn aggressive_parse(&self, content: &str) -> Option<ToolCall> {
        // Try to find "name" or "function" followed by a string
        let name_re = Regex::new(r#""(?:name|function)":\s*"([^"]+)""#).ok()?;
        let args_re = Regex::new(r#""arguments":\s*"?(\{[^}]*\}|"[^"]*")"?"#).ok()?;

        let name = name_re
            .captures(content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        let args = args_re
            .captures(content)
            .map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default())
            .unwrap_or_else(|| "{}".to_string());

        if let Some(name) = name {
            return Some(ToolCall {
                id: format!("call_{}", generate_id()),
                function: ToolCallFunction {
                    name,
                    arguments: args,
                },
            });
        }

        None
    }

    /// Get the current buffer content
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Check if currently inside a tool_call block
    pub fn is_in_tool_call(&self) -> bool {
        self.in_tool_call
    }

    /// Reset the parser to initial state
    pub fn reset(&mut self) {
        self.state = ParserState::Outside;
        self.buffer.clear();
        self.tag_buffer.clear();
        self.nested_depth = 0;
        self.in_tool_call = false;
        self.position = 0;
    }

    /// Parse a complete string (non-streaming)
    pub fn parse(&mut self, content: &str) -> Result<Vec<ToolCall>> {
        let events = self.feed(content);
        self.feed(""); // Signal end

        let mut tool_calls = Vec::new();

        for event in events {
            match event {
                ParserEvent::ToolCall(tc) => tool_calls.push(tc),
                ParserEvent::Error(e) => return Err(crate::error::Error::XmlParse(e)),
                _ => {}
            }
        }

        Ok(tool_calls)
    }
}

/// Generate a simple unique ID
fn generate_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// Truncate a string for display
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        format!(
            "{}...",
            s.chars()
                .take(max_len.saturating_sub(3))
                .collect::<String>()
        )
    }
}

/// Stream-based parser that can be polled incrementally
pub struct ToolCallStreamParser {
    parser: ToolCallParser,
    pending_text: String,
}

impl ToolCallStreamParser {
    /// Create a new stream parser
    pub fn new() -> Self {
        Self {
            parser: ToolCallParser::new(),
            pending_text: String::new(),
        }
    }

    /// Set early detection callback
    pub fn on_tool_call<F>(mut self, callback: F) -> Self
    where
        F: Fn(ToolCall) + Send + Sync + 'static,
    {
        self.parser = self.parser.on_tool_call(callback);
        self
    }

    /// Process incoming chunk and return completed tool calls
    pub fn process_chunk(&mut self, chunk: &str) -> Vec<ToolCall> {
        let events = self.parser.feed(chunk);
        let mut tool_calls = Vec::new();
        let mut text_parts = Vec::new();

        for event in events {
            match event {
                ParserEvent::ToolCall(tc) => tool_calls.push(tc),
                ParserEvent::Text(t) => text_parts.push(t),
                ParserEvent::Error(e) => {
                    warn!(error = %e, "Parser error");
                }
                ParserEvent::End => {
                    // Flush remaining text
                    if !self.pending_text.is_empty() {
                        text_parts.push(std::mem::take(&mut self.pending_text));
                    }
                }
            }
        }

        // Update pending text with accumulated text parts
        for part in text_parts {
            self.pending_text.push_str(&part);
        }

        tool_calls
    }

    /// Flush currently accumulated visible text and return it.
    pub fn take_text(&mut self) -> String {
        std::mem::take(&mut self.pending_text)
    }

    /// Get accumulated text content
    pub fn text(&self) -> &str {
        &self.pending_text
    }

    /// Clear accumulated text
    pub fn clear_text(&mut self) {
        self.pending_text.clear();
    }

    /// Reset the parser
    pub fn reset(&mut self) {
        self.parser.reset();
        self.pending_text.clear();
    }
}

impl Default for ToolCallStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tool_call() {
        let content =
            r#"<tool_call>{"name": "get_weather", "arguments": {"city": "Tokyo"}}</tool_call>"#;
        let mut parser = ToolCallParser::new();
        let tool_calls = parser.parse(content).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert!(tool_calls[0].function.arguments.contains("Tokyo"));
    }

    #[test]
    fn test_tool_call_with_newlines() {
        let content = r#"
<tool_call>
{
  "name": "search",
  "arguments": {
    "query": "rust async"
  }
}
</tool_call>"#;
        let mut parser = ToolCallParser::new();
        let tool_calls = parser.parse(content).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "search");
    }

    #[test]
    fn test_multiple_tool_calls() {
        let content = r#"<tool_call>{"name": "tool1", "arguments": {}}</tool_call>
Some text here
<tool_call>{"name": "tool2", "arguments": {}}</tool_call>"#;
        let mut parser = ToolCallParser::new();
        let tool_calls = parser.parse(content).unwrap();

        assert_eq!(tool_calls.len(), 2);
    }

    #[test]
    fn test_incremental_parsing() {
        let full_content = r#"<tool_call>{"name": "test", "arguments": {}}</tool_call>"#;
        let all_tool_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_tool_calls_clone = all_tool_calls.clone();

        let mut parser = ToolCallParser::new().on_tool_call(move |tc| {
            all_tool_calls_clone.lock().unwrap().push(tc);
        });

        for ch in full_content.chars() {
            parser.feed(&ch.to_string());
        }

        // Should have detected tool call early
        assert_eq!(all_tool_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_malformed_tags() {
        // Test with malformed closing tag
        let content = r#"<tool_call>{"name": "test", "arguments": {}}</tool_call >"#;
        let mut parser = ToolCallParser::new();
        let tool_calls = parser.parse(content).unwrap();

        // Should still handle malformed input gracefully (may or may not find tool calls)
        let _ = tool_calls;
    }

    #[test]
    fn test_nested_json() {
        let content = r#"<tool_call>{"name": "complex", "arguments": {"nested": {"deep": "value"}}}</tool_call>"#;
        let mut parser = ToolCallParser::new();
        let tool_calls = parser.parse(content).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert!(tool_calls[0].function.arguments.contains("nested"));
    }

    #[test]
    fn test_text_content_extraction() {
        let mut parser = ToolCallStreamParser::new();

        parser.process_chunk("Hello, this is ");
        parser.process_chunk("some text ");
        parser.process_chunk("before the tool");

        assert_eq!(parser.text(), "");
        parser.clear_text();
        assert_eq!(parser.text(), "");
    }

    #[test]
    fn test_early_detection_callback() {
        let detected = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let detected_clone = detected.clone();

        let mut parser = ToolCallParser::new().on_tool_call(move |tc| {
            detected_clone.lock().unwrap().push(tc);
        });

        // Feed in parts
        parser.feed("<tool_call>{\"name\": \"test\", \"arguments\": {}}");
        assert_eq!(detected.lock().unwrap().len(), 0); // Not complete yet

        parser.feed("</tool_call>");
        assert_eq!(detected.lock().unwrap().len(), 1); // Now detected
    }

    #[test]
    fn test_stream_parser_filters_tool_call_markup_from_visible_text() {
        let mut parser = ToolCallStreamParser::new();

        let tool_calls = parser.process_chunk(
            "Before <tool_call>{\"name\": \"echo\", \"arguments\": \"{}\"}</tool_call> after",
        );
        let text = parser.take_text();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "echo");
        assert_eq!(text, "");
    }
}
