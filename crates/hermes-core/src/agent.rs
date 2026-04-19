//! Hermes Agent orchestration loop with self-healing
//!
//! Implements the ReAct (Reason + Act) pattern for LLM-driven tool execution.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, instrument, warn};

use crate::client::{ChatStreamEvent, ChatStreamResponse, Message, OpenAIClient, ToolCall};
use crate::error::{Error, Result};
use crate::parser::{ToolCallParser, ToolCallStreamParser};
use crate::tools::{ToolContext, ToolRegistry, ToolResult};

/// Configuration for the Hermes agent
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Model to use (e.g., "gpt-4", "gpt-3.5-turbo")
    pub model: String,
    /// Maximum iterations before giving up
    pub max_iterations: usize,
    /// Timeout for tool execution
    pub tool_timeout: Duration,
    /// Timeout for LLM requests
    pub request_timeout: Duration,
    /// System prompt for the agent
    pub system_prompt: Option<String>,
    /// Whether to stream responses
    pub stream: bool,
    /// Context window size for truncation
    pub context_window: usize,
    /// Max self-healing attempts on tool errors
    pub max_healing_attempts: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "gpt-4".to_string(),
            max_iterations: 20,
            tool_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(120),
            system_prompt: None,
            stream: true,
            context_window: 128_000,
            max_healing_attempts: 3,
        }
    }
}

/// Events emitted by the agent
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Thinking/reasoning step
    Thinking { content: String },
    /// Model reasoning content
    Reasoning { text: String },
    /// Tool execution started
    ToolStart { name: String, arguments: String },
    /// Tool execution completed
    ToolComplete { result: ToolResult },
    /// Tool execution failed
    ToolError { name: String, error: String },
    /// Response content received
    Content { text: String },
    /// Agent finished with final response
    Done { message: Message },
    /// Agent iteration completed
    IterationComplete { iteration: usize },
    /// Agent error
    Error { error: String },
}

/// Hermes Agent for tool orchestration
pub struct HermesAgent {
    config: AgentConfig,
    client: OpenAIClient,
    registry: ToolRegistry,
    conversation: Arc<RwLock<Vec<Message>>>,
    event_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl HermesAgent {
    /// Create a new Hermes agent
    pub fn new(config: AgentConfig, client: OpenAIClient, registry: ToolRegistry) -> Self {
        Self {
            config,
            client,
            registry,
            conversation: Arc::new(RwLock::new(Vec::new())),
            event_tx: None,
        }
    }

    /// Create with event channel for streaming events
    pub fn with_events(
        config: AgentConfig,
        client: OpenAIClient,
        registry: ToolRegistry,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Self {
        Self {
            config,
            client,
            registry,
            conversation: Arc::new(RwLock::new(Vec::new())),
            event_tx: Some(event_tx),
        }
    }

    /// Send an event to the channel
    async fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event).await;
        }
    }

    /// Add a message to the conversation history
    pub async fn add_message(&self, message: Message) {
        let mut conv = self.conversation.write().await;
        conv.push(message);
    }

    /// Add a user message
    pub async fn user_message(&self, content: impl Into<String>) {
        self.add_message(Message::user(content)).await;
    }

    /// Get current conversation
    pub async fn conversation(&self) -> Vec<Message> {
        self.conversation.read().await.clone()
    }

    /// Clear conversation history
    pub async fn clear_history(&self) {
        let mut conv = self.conversation.write().await;
        conv.clear();
    }

    /// Run the agent with a user query
    #[instrument(skip(self), fields(model = % self.config.model))]
    pub async fn run(&self, user_query: String) -> Result<Message> {
        info!("Starting agent run");

        // Add user message
        self.add_message(Message::user(&user_query)).await;

        // Build initial messages including system prompt
        let mut messages = self.build_messages().await?;
        let mut iteration = 0;

        loop {
            iteration += 1;
            debug!(iteration, "Agent iteration");

            if iteration > self.config.max_iterations {
                error!(max = self.config.max_iterations, "Max iterations exceeded");
                return Err(Error::MaxIterationsExceeded {
                    max: self.config.max_iterations,
                });
            }

            // Emit thinking event
            self.emit(AgentEvent::Thinking {
                content: format!(
                    "Iteration {}/{}: Requesting LLM response...",
                    iteration, self.config.max_iterations
                ),
            })
            .await;

            // Get tool schemas
            let tools = self.registry.get_schemas().await;

            // Make streaming request
            let stream = self
                .client
                .chat_streaming(&self.config.model, &messages, Some(&tools))
                .await?;

            // Process streaming response with early tool detection
            match self.process_stream(stream).await {
                Ok((response_text, reasoning_text, tool_calls)) => {
                    // Add assistant message to conversation
                    let mut assistant_msg = Message::assistant(&response_text);
                    if !reasoning_text.is_empty() {
                        assistant_msg = assistant_msg.with_reasoning(reasoning_text);
                    }
                    if !tool_calls.is_empty() {
                        assistant_msg = assistant_msg.with_tool_calls(tool_calls.clone());
                    }

                    messages.push(assistant_msg.clone());
                    self.add_message(assistant_msg.clone()).await;

                    // If no tool calls, we're done
                    if tool_calls.is_empty() {
                        let result = assistant_msg.clone();
                        self.emit(AgentEvent::Done {
                            message: assistant_msg,
                        })
                        .await;
                        return Ok(result);
                    }

                    // Execute tools and add results
                    let tool_results = self.execute_tools(tool_calls).await?;

                    for result in &tool_results {
                        if result.success {
                            self.emit(AgentEvent::ToolComplete {
                                result: result.clone(),
                            })
                            .await;
                        } else {
                            self.emit(AgentEvent::ToolError {
                                name: result.tool_call_id.clone(),
                                error: result.error.clone().unwrap_or_default(),
                            })
                            .await;
                        }
                    }

                    // Add tool results to messages
                    for result in tool_results {
                        messages.push(Message::tool(
                            &result.tool_call_id,
                            if result.success {
                                &result.content
                            } else {
                                result.error.as_deref().unwrap_or("Error")
                            },
                        ));
                    }
                }
                Err(e) => {
                    error!(error = %e, "Error processing stream");
                    self.emit(AgentEvent::Error {
                        error: e.to_string(),
                    })
                    .await;
                    return Err(e);
                }
            }

            self.emit(AgentEvent::IterationComplete { iteration }).await;
        }
    }

    /// Build messages including system prompt
    async fn build_messages(&self) -> Result<Vec<Message>> {
        let mut messages = Vec::new();

        // Add system prompt
        if let Some(ref system) = self.config.system_prompt {
            messages.push(Message::system(system));
        } else {
            // Default system prompt
            messages.push(Message::system(
                "You are Hermes, an AI assistant that uses tools to help users. \
                When you need to use a tool, output your request in the following XML format:\n\
                <tool_call>{\"name\": \"tool_name\", \"arguments\": {\"arg1\": \"value1\"}}</tool_call>\n\
                If you need to use multiple tools, output them sequentially, each wrapped in its own XML tags.\n\
                After receiving tool results, continue reasoning and either call more tools or provide your final response."
            ));
        }

        // Add conversation history
        let conv = self.conversation.read().await;
        messages.extend(conv.clone());

        Ok(messages)
    }

    /// Process streaming response with early tool detection
    async fn process_stream(
        &self,
        mut stream: ChatStreamResponse,
    ) -> Result<(String, String, Vec<ToolCall>)> {
        let _parser = ToolCallStreamParser::new().on_tool_call(|tc| {
            let tc_id = tc.id.clone();
            debug!(tool_call_id = %tc_id, name = %tc.function.name, "Early tool call detected");
        });
        let mut content_router = ThinkBlockRouter::default();
        let mut accumulated_text = String::new();
        let mut accumulated_reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut has_error = false;

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => {
                    // Process the event
                    if let Some(reasoning) = extract_reasoning_from_event(&event) {
                        let reasoning = strip_reasoning_tags(&reasoning);
                        if !reasoning.is_empty() {
                            accumulated_reasoning.push_str(&reasoning);
                            self.emit(AgentEvent::Reasoning { text: reasoning }).await;
                        }
                    }

                    if let Some(text) = extract_text_from_event(&event) {
                        let (content_delta, reasoning_delta) = content_router.feed(&text);

                        if !content_delta.is_empty() {
                            accumulated_text.push_str(&content_delta);
                            self.emit(AgentEvent::Content {
                                text: content_delta,
                            })
                            .await;
                        }

                        if !reasoning_delta.is_empty() {
                            accumulated_reasoning.push_str(&reasoning_delta);
                            self.emit(AgentEvent::Reasoning {
                                text: reasoning_delta,
                            })
                            .await;
                        }
                    }

                    // Extract any tool calls from this chunk
                    let chunk_tool_calls = extract_tool_calls_from_event(&event);
                    for tc in chunk_tool_calls {
                        tool_calls.push(tc);
                    }
                }
                Err(e) => {
                    error!(error = %e, "Stream error");
                    has_error = true;
                    break;
                }
            }
        }

        if has_error {
            return Err(Error::Agent("Stream processing failed".to_string()));
        }

        let (remaining_content, remaining_reasoning) = content_router.finish();
        accumulated_text.push_str(&remaining_content);
        accumulated_reasoning.push_str(&remaining_reasoning);

        // Also try to extract any remaining tool calls from accumulated text
        let mut remaining_parser = ToolCallParser::new();
        let remaining_calls = remaining_parser.parse(&accumulated_text)?;

        // Merge tool calls, avoiding duplicates
        for tc in remaining_calls {
            if !tool_calls.iter().any(|existing| existing.id == tc.id) {
                tool_calls.push(tc);
            }
        }

        Ok((accumulated_text, accumulated_reasoning, tool_calls))
    }

    /// Execute tools and handle self-healing
    async fn execute_tools(&self, tool_calls: Vec<ToolCall>) -> Result<Vec<ToolResult>> {
        let mut results = Vec::new();

        for tool_call in tool_calls {
            let name = tool_call.function.name.clone();
            let args_str = tool_call.function.arguments.clone();

            debug!(tool = %name, args = %args_str, "Executing tool");
            self.emit(AgentEvent::ToolStart {
                name: name.clone(),
                arguments: args_str.clone(),
            })
            .await;

            // Parse arguments
            let args: serde_json::Value = match serde_json::from_str(&args_str) {
                Ok(a) => a,
                Err(e) => {
                    warn!(tool = %name, error = %e, "Failed to parse tool arguments");
                    results.push(ToolResult::error(
                        &tool_call.id,
                        format!("Invalid JSON: {}", e),
                    ));
                    continue;
                }
            };

            // Validate tool exists
            if !self.registry.contains(&name).await {
                error!(tool = %name, "Tool not found");
                results.push(ToolResult::error(
                    &tool_call.id,
                    format!("Tool '{}' not found", name),
                ));
                continue;
            }

            // Execute with timeout
            let result = timeout(
                self.config.tool_timeout,
                self.registry
                    .execute(&name, &tool_call.id, args, ToolContext::default()),
            )
            .await;

            match result {
                Ok(Ok(r)) => {
                    debug!(tool = %name, success = r.success, "Tool execution completed");
                    results.push(r);
                }
                Ok(Err(e)) => {
                    error!(tool = %name, error = %e, "Tool execution failed");
                    results.push(ToolResult::error(&tool_call.id, e.to_string()));
                }
                Err(_) => {
                    error!(tool = %name, "Tool execution timed out");
                    results.push(ToolResult::error(
                        &tool_call.id,
                        format!("Tool timed out after {:?}", self.config.tool_timeout),
                    ));
                }
            }
        }

        Ok(results)
    }

    /// Run agent and handle self-healing on tool errors
    pub async fn run_with_healing(&self, user_query: String) -> Result<Message> {
        let mut iteration = 0;
        let max_healing_attempts = self.config.max_healing_attempts;

        loop {
            iteration += 1;

            match self.run(user_query.clone()).await {
                Ok(response) => return Ok(response),
                Err(e) if e.is_self_healing() && iteration <= max_healing_attempts => {
                    warn!(iteration, error = %e, "Self-healing: re-prompting LLM");

                    // Add error context as a system message
                    let error_msg = format!(
                        "Note: The previous attempt encountered an error: {}. \
                        Please correct your approach and try again.",
                        e.user_message()
                    );

                    self.add_message(Message::system(&error_msg)).await;
                }
                Err(e) => {
                    error!(error = %e, "Agent run failed");
                    return Err(e);
                }
            }
        }
    }
}

/// Extract text content from a streaming event
fn extract_text_from_event(event: &ChatStreamEvent) -> Option<String> {
    let mut text = String::new();

    for choice in &event.choices {
        if let Some(content) = &choice.delta.content {
            text.push_str(content);
        }
    }

    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Extract reasoning content from a streaming event
fn extract_reasoning_from_event(event: &ChatStreamEvent) -> Option<String> {
    let mut reasoning = String::new();

    for choice in &event.choices {
        if let Some(content) = &choice.delta.reasoning_content {
            reasoning.push_str(content);
        }
    }

    if reasoning.is_empty() {
        None
    } else {
        Some(reasoning)
    }
}

#[derive(Debug, Default)]
struct ThinkBlockRouter {
    pending: String,
    inside_reasoning: bool,
}

impl ThinkBlockRouter {
    fn feed(&mut self, chunk: &str) -> (String, String) {
        self.pending.push_str(chunk);
        self.drain_ready()
    }

    fn finish(&mut self) -> (String, String) {
        let (mut content, mut reasoning) = self.drain_ready();
        if !self.pending.is_empty() {
            if self.inside_reasoning {
                reasoning.push_str(&self.pending);
            } else {
                content.push_str(&self.pending);
            }
            self.pending.clear();
        }
        (content, reasoning)
    }

    fn drain_ready(&mut self) -> (String, String) {
        const MAX_TAG_LEN: usize = 23;
        let mut content = String::new();
        let mut reasoning = String::new();

        loop {
            let lowered = self.pending.to_ascii_lowercase();
            let tag = if self.inside_reasoning {
                find_first_tag(&lowered, CLOSE_REASONING_TAGS)
            } else {
                find_first_tag(&lowered, OPEN_REASONING_TAGS)
            };

            if let Some((index, marker)) = tag {
                let segment = self.pending[..index].to_string();
                if self.inside_reasoning {
                    reasoning.push_str(&segment);
                } else {
                    content.push_str(&segment);
                }
                self.pending.drain(..index + marker.len());
                self.inside_reasoning = !self.inside_reasoning;
                continue;
            }

            let keep = self.pending.len().min(MAX_TAG_LEN.saturating_sub(1));
            let flush_len = self.pending.len().saturating_sub(keep);
            if flush_len == 0 {
                break;
            }

            let segment = self.pending[..flush_len].to_string();
            if self.inside_reasoning {
                reasoning.push_str(&segment);
            } else {
                content.push_str(&segment);
            }
            self.pending.drain(..flush_len);
        }

        (content, reasoning)
    }
}

const OPEN_REASONING_TAGS: &[&str] = &[
    "<think>",
    "<thinking>",
    "<reasoning>",
    "<thought>",
    "<reasoning_scratchpad>",
];

const CLOSE_REASONING_TAGS: &[&str] = &[
    "</think>",
    "</thinking>",
    "</reasoning>",
    "</thought>",
    "</reasoning_scratchpad>",
];

fn find_first_tag<'a>(haystack: &str, tags: &'a [&'a str]) -> Option<(usize, &'a str)> {
    tags.iter()
        .filter_map(|tag| haystack.find(tag).map(|index| (index, *tag)))
        .min_by_key(|(index, _)| *index)
}

fn strip_reasoning_tags(text: &str) -> String {
    let mut cleaned = text.to_string();
    for tag in OPEN_REASONING_TAGS
        .iter()
        .chain(CLOSE_REASONING_TAGS.iter())
    {
        cleaned = cleaned.replace(tag, "");
        cleaned = cleaned.replace(&tag.to_uppercase(), "");
    }
    cleaned
}

/// Extract tool calls from a streaming event
fn extract_tool_calls_from_event(event: &ChatStreamEvent) -> Vec<ToolCall> {
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for choice in &event.choices {
        if let Some(delta_tool_calls) = &choice.delta.tool_calls {
            for delta in delta_tool_calls {
                if let Some(ref function) = delta.function {
                    // Extract the tool call ID
                    let id = delta
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("call_{}", tool_calls.len()));

                    // Create or update tool call
                    if let Some(last) = tool_calls.last_mut() {
                        if last.id == id {
                            // Append to existing
                            last.function.arguments.push_str(&function.arguments);
                            continue;
                        }
                    }

                    // New tool call
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        function: crate::client::ToolCallFunction {
                            name: function.name.clone(),
                            arguments: function.arguments.clone(),
                        },
                    });
                }
            }
        }
    }

    tool_calls
}

/// Builder for creating a HermesAgent
pub struct HermesAgentBuilder {
    config: AgentConfig,
    client: Option<OpenAIClient>,
    registry: Option<ToolRegistry>,
}

impl HermesAgentBuilder {
    pub fn new() -> Self {
        Self {
            config: AgentConfig::default(),
            client: None,
            registry: None,
        }
    }

    /// Set the model
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.config.model = model.into();
        self
    }

    /// Set maximum iterations
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.config.max_iterations = max;
        self
    }

    /// Set tool timeout
    pub fn tool_timeout(mut self, timeout: Duration) -> Self {
        self.config.tool_timeout = timeout;
        self
    }

    /// Set request timeout
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.config.request_timeout = timeout;
        self
    }

    /// Set system prompt
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(prompt.into());
        self
    }

    /// Enable/disable streaming
    pub fn streaming(mut self, enabled: bool) -> Self {
        self.config.stream = enabled;
        self
    }

    /// Set the OpenAI client
    pub fn client(mut self, client: OpenAIClient) -> Self {
        self.client = Some(client);
        self
    }

    /// Set the tool registry
    pub fn registry(mut self, registry: ToolRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Build the agent
    pub fn build(self) -> Result<HermesAgent> {
        let client = self.client.unwrap_or_else(|| {
            OpenAIClient::from_env()
                .unwrap_or_else(|_| OpenAIClient::new(crate::client::ClientConfig::default()))
        });

        let registry = self
            .registry
            .unwrap_or_else(|| ToolRegistry::new(self.config.tool_timeout));

        Ok(HermesAgent::new(self.config, client, registry))
    }
}

impl Default for HermesAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AgentConfig::default();
        assert_eq!(config.model, "gpt-4");
        assert_eq!(config.max_iterations, 20);
    }

    #[tokio::test]
    async fn test_agent_builder() {
        let _agent = HermesAgentBuilder::new()
            .model("gpt-3.5-turbo")
            .max_iterations(10)
            .build()
            .unwrap();

        // If we reach here, the agent was created successfully
    }

    #[test]
    fn test_extract_text_from_event() {
        let event = ChatStreamEvent {
            id: "test".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "test".to_string(),
            choices: vec![crate::client::StreamChoice {
                index: 0,
                delta: crate::client::StreamingMessageDelta {
                    role: None,
                    content: Some("Hello ".to_string()),
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
        };

        let text = extract_text_from_event(&event);
        assert_eq!(text, Some("Hello ".to_string()));
    }

    #[test]
    fn think_router_splits_inline_think_blocks() {
        let mut router = ThinkBlockRouter::default();
        let (content_a, reasoning_a) = router.feed("Hello<think>plan");
        let (content_b, reasoning_b) = router.feed(" more</think> world");
        let (content_c, reasoning_c) = router.finish();

        assert_eq!(content_a, "Hello");
        assert_eq!(reasoning_a, "");
        assert_eq!(content_b, "");
        assert_eq!(reasoning_b, "plan more");
        assert_eq!(content_c, " world");
        assert_eq!(reasoning_c, "");
    }

    #[test]
    fn strip_reasoning_tags_removes_supported_markers() {
        assert_eq!(
            strip_reasoning_tags(
                "<think>abc</think><REASONING_SCRATCHPAD>def</REASONING_SCRATCHPAD>"
            ),
            "abcdef"
        );
    }
}
