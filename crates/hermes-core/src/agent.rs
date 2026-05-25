//! Hermes Agent orchestration loop with self-healing
//!
//! Implements the ReAct (Reason + Act) pattern for LLM-driven tool execution.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, instrument, warn};

use crate::client::{
    ChatResponse, ChatStreamEvent, ChatStreamResponse, Message, OpenAIClient, ToolCall,
};
use crate::config::{runtime_config, BehaviorSettings};
use crate::context::{estimate_message_tokens, estimate_tokens};
use crate::context_files::{load_default_context_files, load_workspace_context};
use crate::distillation::distill_session_to_memory;
use crate::error::{Error, Result};
use crate::memory::MemoryManager;
use crate::parser::{ToolCallParser, ToolCallStreamParser};
use crate::schema::ToolSchema;
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
        Self::from(&runtime_config().agent)
    }
}

impl From<&BehaviorSettings> for AgentConfig {
    fn from(settings: &BehaviorSettings) -> Self {
        Self {
            model: settings.model.clone(),
            max_iterations: settings.max_iterations,
            tool_timeout: Duration::from_secs(settings.tool_timeout_secs),
            request_timeout: Duration::from_secs(settings.request_timeout_secs),
            system_prompt: settings.system_prompt.clone(),
            stream: settings.stream,
            context_window: settings.context_window,
            max_healing_attempts: settings.max_healing_attempts,
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
    /// Token, context, and compaction telemetry
    Telemetry { telemetry: AgentTelemetry },
    /// Agent error
    Error { error: String },
}

#[derive(Debug, Clone)]
pub struct AgentTelemetry {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub context_window: usize,
    pub compacted: bool,
    pub estimated: bool,
    pub billable: bool,
}

/// Hermes Agent for tool orchestration
pub struct HermesAgent {
    config: AgentConfig,
    client: OpenAIClient,
    registry: ToolRegistry,
    conversation: Arc<RwLock<Vec<Message>>>,
    event_tx: Option<mpsc::Sender<AgentEvent>>,
    memory_manager: Option<MemoryManager>,
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
            memory_manager: None,
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
            memory_manager: None,
        }
    }

    /// Attach a memory manager for long-term memory injection and session distillation.
    pub fn with_memory_manager(mut self, memory_manager: MemoryManager) -> Self {
        self.memory_manager = Some(memory_manager);
        self
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
            let (request_messages, preflight_telemetry) =
                self.prepare_request_messages(&messages, &tools)?;
            self.emit(AgentEvent::Telemetry {
                telemetry: preflight_telemetry.clone(),
            })
            .await;

            let response = if self.config.stream {
                let stream = self
                    .client
                    .chat_streaming(&self.config.model, &request_messages, Some(&tools))
                    .await?;
                match self.process_stream(stream, &preflight_telemetry).await {
                    Ok((response_text, reasoning_text, tool_calls)) => {
                        self.emit_stream_telemetry(
                            &preflight_telemetry,
                            &response_text,
                            &reasoning_text,
                            &tool_calls,
                        )
                        .await;
                        Ok((response_text, reasoning_text, tool_calls))
                    }
                    Err(error) => Err(error),
                }
            } else {
                let response = self
                    .client
                    .chat(&self.config.model, &request_messages, Some(&tools))
                    .await?;
                self.process_response(response, &preflight_telemetry).await
            };

            match response {
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
                        self.spawn_session_distillation(messages.clone());
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

        let mut system_prompt = if let Some(ref system) = self.config.system_prompt {
            system.clone()
        } else {
            "You are Hermes, an AI assistant that uses tools to help users. \
                When you need to use a tool, output your request in the following XML format:\n\
                <tool_call>{\"name\": \"tool_name\", \"arguments\": {\"arg1\": \"value1\"}}</tool_call>\n\
                If you need to use multiple tools, output them sequentially, each wrapped in its own XML tags.\n\
                After receiving tool results, continue reasoning and either call more tools or provide your final response."
                .to_string()
        };

        if let Some(memory_manager) = &self.memory_manager {
            let memory_context = memory_manager.build_memory_context(2048).await;
            let memory_context = memory_context.trim();
            if !memory_context.is_empty() {
                system_prompt.push_str("\n\n<long_term_memory>\n");
                system_prompt.push_str(memory_context);
                system_prompt.push_str("\n</long_term_memory>");
            }
        }

        let context_files = self.load_context_file_prompt();
        if !context_files.trim().is_empty() {
            system_prompt.push_str("\n\n<workspace_context>\n");
            system_prompt.push_str(context_files.trim());
            system_prompt.push_str("\n</workspace_context>");
        }

        // Add system prompt
        messages.push(Message::system(system_prompt));

        // Add conversation history
        let conv = self.conversation.read().await;
        messages.extend(conv.clone());

        Ok(messages)
    }

    fn load_context_file_prompt(&self) -> String {
        let mut blocks = Vec::new();

        let global_context = load_default_context_files();
        if !global_context.trim().is_empty() {
            blocks.push(global_context);
        }

        match std::env::current_dir() {
            Ok(cwd) => {
                if let Some(workspace_context) = load_workspace_context(&cwd) {
                    blocks.push(workspace_context);
                }
            }
            Err(error) => {
                warn!(error = %error, "Could not determine current directory for context files")
            }
        }

        blocks.join("\n\n")
    }

    fn prepare_request_messages(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
    ) -> Result<(Vec<Message>, AgentTelemetry)> {
        let context_window = self.config.context_window.max(1);
        let tool_tokens = total_tool_schema_tokens(tools);
        let prompt_tokens = total_message_tokens(messages) + tool_tokens;
        let compacted = prompt_tokens > context_window;
        let message_budget = context_window.saturating_sub(tool_tokens).max(1);
        let request_messages = if compacted {
            compact_request_messages(messages, message_budget)
        } else {
            messages.to_vec()
        };
        let prompt_tokens = total_message_tokens(&request_messages) + tool_tokens;

        Ok((
            request_messages,
            AgentTelemetry {
                prompt_tokens,
                completion_tokens: 0,
                total_tokens: prompt_tokens,
                context_window,
                compacted,
                estimated: true,
                billable: false,
            },
        ))
    }

    async fn emit_stream_telemetry(
        &self,
        preflight: &AgentTelemetry,
        response_text: &str,
        reasoning_text: &str,
        tool_calls: &[ToolCall],
    ) {
        let completion_tokens = estimate_tokens(response_text)
            + estimate_tokens(reasoning_text)
            + total_tool_call_tokens(tool_calls);
        self.emit(AgentEvent::Telemetry {
            telemetry: AgentTelemetry {
                prompt_tokens: preflight.prompt_tokens,
                completion_tokens,
                total_tokens: preflight.prompt_tokens + completion_tokens,
                context_window: preflight.context_window,
                compacted: preflight.compacted,
                estimated: true,
                billable: true,
            },
        })
        .await;
    }

    async fn emit_stream_telemetry_snapshot(
        &self,
        preflight: &AgentTelemetry,
        response_text: &str,
        reasoning_text: &str,
        tool_calls: &[ToolCall],
    ) {
        let completion_tokens = estimate_tokens(response_text)
            + estimate_tokens(reasoning_text)
            + total_tool_call_tokens(tool_calls);
        self.emit(AgentEvent::Telemetry {
            telemetry: AgentTelemetry {
                prompt_tokens: preflight.prompt_tokens,
                completion_tokens,
                total_tokens: preflight.prompt_tokens + completion_tokens,
                context_window: preflight.context_window,
                compacted: preflight.compacted,
                estimated: true,
                billable: false,
            },
        })
        .await;
    }

    fn spawn_session_distillation(&self, history: Vec<Message>) {
        let Some(memory_manager) = self.memory_manager.clone() else {
            return;
        };

        let client = self.client.clone();
        let model = self.config.model.clone();
        tokio::spawn(async move {
            if let Err(error) =
                distill_session_to_memory(client, model, memory_manager, history).await
            {
                warn!(error = %error, "Session distillation failed");
            }
        });
    }

    /// Process streaming response with early tool detection
    async fn process_stream(
        &self,
        mut stream: ChatStreamResponse,
        preflight: &AgentTelemetry,
    ) -> Result<(String, String, Vec<ToolCall>)> {
        let mut parser = ToolCallStreamParser::new().on_tool_call(|tc| {
            let tc_id = tc.id.clone();
            debug!(tool_call_id = %tc_id, name = %tc.function.name, "Early tool call detected");
        });
        let mut content_router = ThinkBlockRouter::default();
        let mut tool_call_router = ToolCallContentRouter::default();
        let mut accumulated_text = String::new();
        let mut accumulated_reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
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
                            self.emit_stream_telemetry_snapshot(
                                preflight,
                                &accumulated_text,
                                &accumulated_reasoning,
                                &tool_calls,
                            )
                            .await;
                        }
                    }

                    if let Some(text) = extract_text_from_event(&event) {
                        let (content_delta, reasoning_delta) = content_router.feed(&text);

                        if !content_delta.is_empty() {
                            let chunk_tool_calls = parser.process_chunk(&content_delta);
                            for tc in chunk_tool_calls {
                                if !tool_calls.iter().any(|existing| existing.id == tc.id) {
                                    tool_calls.push(tc);
                                }
                            }

                            let visible_text = tool_call_router.feed(&content_delta);
                            if !visible_text.is_empty() {
                                accumulated_text.push_str(&visible_text);
                                self.emit(AgentEvent::Content { text: visible_text }).await;
                                self.emit_stream_telemetry_snapshot(
                                    preflight,
                                    &accumulated_text,
                                    &accumulated_reasoning,
                                    &tool_calls,
                                )
                                .await;
                            }
                        }

                        if !reasoning_delta.is_empty() {
                            accumulated_reasoning.push_str(&reasoning_delta);
                            self.emit(AgentEvent::Reasoning {
                                text: reasoning_delta,
                            })
                            .await;
                            self.emit_stream_telemetry_snapshot(
                                preflight,
                                &accumulated_text,
                                &accumulated_reasoning,
                                &tool_calls,
                            )
                            .await;
                        }
                    }

                    // Extract any tool calls from native provider tool-call deltas
                    let chunk_tool_calls = extract_tool_calls_from_event(&event);
                    let had_native_tool_calls = !chunk_tool_calls.is_empty();
                    for tc in chunk_tool_calls {
                        merge_stream_tool_call(&mut tool_calls, tc);
                    }
                    if had_native_tool_calls {
                        self.emit_stream_telemetry_snapshot(
                            preflight,
                            &accumulated_text,
                            &accumulated_reasoning,
                            &tool_calls,
                        )
                        .await;
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
        if !remaining_content.is_empty() {
            let remaining_calls = parser.process_chunk(&remaining_content);
            for tc in remaining_calls {
                merge_stream_tool_call(&mut tool_calls, tc);
            }
            accumulated_text.push_str(&tool_call_router.feed(&remaining_content));
        }
        accumulated_text.push_str(&tool_call_router.finish());
        accumulated_reasoning.push_str(&remaining_reasoning);

        // Also try to extract any remaining tool calls from accumulated text
        let mut remaining_parser = ToolCallParser::new();
        let remaining_calls = remaining_parser.parse(&accumulated_text)?;

        // Merge tool calls, avoiding duplicates
        for tc in remaining_calls {
            merge_stream_tool_call(&mut tool_calls, tc);
        }

        Ok((accumulated_text, accumulated_reasoning, tool_calls))
    }

    async fn process_response(
        &self,
        response: ChatResponse,
        preflight: &AgentTelemetry,
    ) -> Result<(String, String, Vec<ToolCall>)> {
        let usage = response.usage.clone();
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::ParseResponse("response had no choices".to_string()))?;

        let message = choice.message;
        let raw_content = message.content.unwrap_or_default();
        let content = strip_tool_call_markup(&raw_content);
        let reasoning = message
            .reasoning_content
            .map(|value| strip_reasoning_tags(&value))
            .unwrap_or_default();
        let mut tool_calls = extract_tool_calls_from_choice(message.tool_calls);
        let mut xml_parser = ToolCallParser::new();
        if let Ok(xml_tool_calls) = xml_parser.parse(&raw_content) {
            for tool_call in xml_tool_calls {
                merge_stream_tool_call(&mut tool_calls, tool_call);
            }
        }

        if !content.is_empty() {
            self.emit(AgentEvent::Content {
                text: content.clone(),
            })
            .await;
        }
        if !reasoning.is_empty() {
            self.emit(AgentEvent::Reasoning {
                text: reasoning.clone(),
            })
            .await;
        }
        self.emit(AgentEvent::Telemetry {
            telemetry: AgentTelemetry {
                prompt_tokens: usage.prompt_tokens as usize,
                completion_tokens: usage.completion_tokens as usize,
                total_tokens: usage.total_tokens as usize,
                context_window: preflight.context_window,
                compacted: preflight.compacted,
                estimated: false,
                billable: true,
            },
        })
        .await;

        Ok((content, reasoning, tool_calls))
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

fn total_message_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

fn total_tool_schema_tokens(tools: &[ToolSchema]) -> usize {
    tools
        .iter()
        .map(|tool| {
            serde_json::to_string(tool)
                .map(|raw| estimate_tokens(&raw))
                .unwrap_or_default()
        })
        .sum()
}

fn total_tool_call_tokens(tool_calls: &[ToolCall]) -> usize {
    tool_calls
        .iter()
        .map(|tool_call| {
            serde_json::to_string(tool_call)
                .map(|raw| estimate_tokens(&raw))
                .unwrap_or_default()
        })
        .sum()
}

fn compact_request_messages(messages: &[Message], max_tokens: usize) -> Vec<Message> {
    if messages.is_empty() {
        return Vec::new();
    }

    let (system, body) = if messages[0].role == crate::client::Role::System {
        (Some(messages[0].clone()), &messages[1..])
    } else {
        (None, messages)
    };

    let mut groups = Vec::<Vec<Message>>::new();
    let mut index = body.len();
    while index > 0 {
        let end = index;
        index -= 1;
        if body[index].role == crate::client::Role::Tool {
            while index > 0 && body[index - 1].role == crate::client::Role::Tool {
                index -= 1;
            }
            if index > 0
                && body[index - 1].role == crate::client::Role::Assistant
                && body[index - 1].tool_calls.is_some()
            {
                index -= 1;
            }
            groups.push(body[index..end].to_vec());
        } else {
            groups.push(vec![body[index].clone()]);
        }
    }

    let latest_user_group = groups.iter().position(|group| {
        group
            .iter()
            .any(|message| message.role == crate::client::Role::User)
    });
    let mut used_tokens = system
        .as_ref()
        .map(estimate_message_tokens)
        .unwrap_or_default();
    let mut selected = Vec::<(usize, Vec<Message>)>::new();
    for (group_index, group) in groups.into_iter().enumerate() {
        let group_tokens = total_message_tokens(&group);
        if selected.is_empty()
            || latest_user_group == Some(group_index)
            || used_tokens + group_tokens <= max_tokens
        {
            used_tokens += group_tokens;
            selected.push((group_index, group));
        } else if latest_user_group.is_some_and(|index| group_index < index) {
            continue;
        } else {
            break;
        }
    }

    let mut compacted = Vec::new();
    if let Some(system) = system {
        compacted.push(system);
    }
    selected.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, group) in selected {
        compacted.extend(group);
    }
    compacted
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
                if content.trim().is_empty() {
                    content.push_str(&self.pending);
                }
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
            let flush_len =
                floor_char_boundary(&self.pending, self.pending.len().saturating_sub(keep));
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

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut boundary = index.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
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
                    let id = delta.id.clone().unwrap_or_else(|| {
                        format!("call_stream_{}_{}", delta.index, function.name)
                    });

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

fn extract_tool_calls_from_choice(
    deltas: Option<Vec<crate::client::ToolCallDelta>>,
) -> Vec<ToolCall> {
    deltas
        .unwrap_or_default()
        .into_iter()
        .filter_map(|delta| {
            let function = delta.function?;
            Some(ToolCall {
                id: delta
                    .id
                    .unwrap_or_else(|| format!("call_choice_{}_{}", delta.index, function.name)),
                function,
            })
        })
        .collect()
}

fn merge_stream_tool_call(tool_calls: &mut Vec<ToolCall>, tool_call: ToolCall) {
    if let Some(existing) = tool_calls
        .iter_mut()
        .find(|existing| existing.id == tool_call.id)
    {
        if existing.function.name.is_empty() {
            existing.function.name = tool_call.function.name;
        }
        if !tool_call.function.arguments.is_empty() {
            if existing.function.arguments == "{}" {
                existing.function.arguments = tool_call.function.arguments;
            } else {
                existing
                    .function
                    .arguments
                    .push_str(&tool_call.function.arguments);
            }
        }
    } else {
        tool_calls.push(tool_call);
    }
}

#[derive(Default)]
struct ToolCallContentRouter {
    pending: String,
    inside_tool_call: bool,
}

impl ToolCallContentRouter {
    fn feed(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        self.drain_ready(false)
    }

    fn finish(&mut self) -> String {
        self.drain_ready(true)
    }

    fn drain_ready(&mut self, flush_all: bool) -> String {
        const OPEN: &str = "<tool_call";
        const CLOSE: &str = "</tool_call";
        let mut content = String::new();

        loop {
            if self.inside_tool_call {
                if let Some(index) = find_ascii_case_insensitive(&self.pending, CLOSE) {
                    let close_end = self.pending[index..]
                        .find('>')
                        .map(|offset| index + offset + 1);
                    if let Some(close_end) = close_end {
                        self.pending.drain(..close_end);
                        self.inside_tool_call = false;
                        continue;
                    }
                }

                if flush_all {
                    self.pending.clear();
                }
                break;
            }

            if let Some(index) = find_ascii_case_insensitive(&self.pending, OPEN) {
                content.push_str(&self.pending[..index]);
                if let Some(open_end) = self.pending[index..]
                    .find('>')
                    .map(|offset| index + offset + 1)
                {
                    self.pending.drain(..open_end);
                    self.inside_tool_call = false;
                    self.inside_tool_call = true;
                    continue;
                }

                self.pending.drain(..index);
                break;
            }

            let keep = if flush_all {
                0
            } else {
                longest_suffix_prefix_match_case_insensitive(&self.pending, OPEN)
            };
            let flush_len = self.pending.len().saturating_sub(keep);
            if flush_len == 0 {
                break;
            }

            content.push_str(&self.pending[..flush_len]);
            self.pending.drain(..flush_len);
            break;
        }

        content
    }
}

fn longest_suffix_prefix_match(value: &str, marker: &str) -> usize {
    let max = value.len().min(marker.len().saturating_sub(1));
    for len in (1..=max).rev() {
        if value.ends_with(&marker[..len]) {
            return len;
        }
    }
    0
}

fn longest_suffix_prefix_match_case_insensitive(value: &str, marker: &str) -> usize {
    let lowered = value.to_ascii_lowercase();
    longest_suffix_prefix_match(&lowered, marker)
}

fn find_ascii_case_insensitive(value: &str, marker: &str) -> Option<usize> {
    value.to_ascii_lowercase().find(marker)
}

fn strip_tool_call_markup(content: &str) -> String {
    let mut router = ToolCallContentRouter::default();
    let mut visible = router.feed(content);
    visible.push_str(&router.finish());
    visible
}

/// Builder for creating a HermesAgent
pub struct HermesAgentBuilder {
    config: AgentConfig,
    client: Option<OpenAIClient>,
    registry: Option<ToolRegistry>,
    memory_manager: Option<MemoryManager>,
}

impl HermesAgentBuilder {
    pub fn new() -> Self {
        Self {
            config: AgentConfig::default(),
            client: None,
            registry: None,
            memory_manager: None,
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

    /// Set the long-term memory manager.
    pub fn memory_manager(mut self, memory_manager: MemoryManager) -> Self {
        self.memory_manager = Some(memory_manager);
        self
    }

    /// Build the agent
    pub fn build(self) -> Result<HermesAgent> {
        let client = match self.client {
            Some(client) => client,
            None => OpenAIClient::from_env()?,
        };

        let registry = self
            .registry
            .unwrap_or_else(|| ToolRegistry::new(self.config.tool_timeout));

        let mut agent = HermesAgent::new(self.config, client, registry);
        if let Some(memory_manager) = self.memory_manager {
            agent = agent.with_memory_manager(memory_manager);
        }

        Ok(agent)
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
    use async_trait::async_trait;
    use serial_test::serial;

    struct WrongIdTool;

    #[async_trait]
    impl crate::tools::HermesTool for WrongIdTool {
        fn name(&self) -> &str {
            "wrong_id"
        }

        fn description(&self) -> &str {
            "Returns a mismatched tool call id"
        }

        fn schema(&self) -> crate::schema::ToolSchema {
            crate::schema::ToolSchema::new(
                "wrong_id",
                "Returns a mismatched tool call id",
                serde_json::json!({ "type": "object", "properties": {} }),
            )
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _context: crate::tools::ToolContext,
        ) -> ToolResult {
            ToolResult::success("internal_tool_id", serde_json::json!({ "ok": true }))
        }
    }

    #[test]
    fn test_default_config() {
        let config = AgentConfig::default();
        assert_eq!(config.model, "gpt-4");
        assert_eq!(config.max_iterations, 20);
    }

    #[tokio::test]
    #[serial]
    async fn test_agent_builder() {
        let _agent = HermesAgentBuilder::new()
            .model("gpt-3.5-turbo")
            .max_iterations(10)
            .build()
            .unwrap();

        // If we reach here, the agent was created successfully
    }

    #[test]
    #[serial]
    fn agent_builder_propagates_auth_profile_errors() {
        let auth_store_path = std::env::temp_dir().join(format!(
            "hermes_agent_auth_error_{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&auth_store_path);
        let old_auth_store = std::env::var("HERMES_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("HERMES_AUTH_REF").ok();
        let old_api_key = std::env::var("OPENAI_API_KEY").ok();

        std::env::set_var("HERMES_AUTH_STORE", &auth_store_path);
        std::env::set_var("HERMES_AUTH_REF", "missing-profile");
        std::env::remove_var("OPENAI_API_KEY");

        let result = HermesAgentBuilder::new().build();

        assert!(result.is_err());

        restore_env("HERMES_AUTH_STORE", old_auth_store);
        restore_env("HERMES_AUTH_REF", old_auth_ref);
        restore_env("OPENAI_API_KEY", old_api_key);
        let _ = std::fs::remove_file(auth_store_path);
    }

    fn restore_env(key: &str, value: Option<String>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    #[tokio::test]
    async fn build_messages_injects_long_term_memory() {
        let memory_manager = MemoryManager::new();
        memory_manager
            .store(
                crate::memory::MemoryBlock::new("fact1", "fact", "User prefers concise answers")
                    .importance(80),
            )
            .await;

        let agent = HermesAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        )
        .with_memory_manager(memory_manager);

        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|message| message.content.as_str())
            .unwrap_or_default();

        assert!(system.contains("<long_term_memory>"));
        assert!(system.contains("[fact] User prefers concise answers"));
        assert!(system.contains("</long_term_memory>"));
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

    #[test]
    fn think_router_does_not_split_multibyte_characters() {
        let mut router = ThinkBlockRouter::default();
        let (_content, _reasoning) = router.feed("Halo! 🧑‍💻 Senang bertemu");
        let (_content, _reasoning) = router.finish();
    }

    #[test]
    fn think_router_falls_back_to_content_for_unclosed_reasoning() {
        let mut router = ThinkBlockRouter::default();
        let (content, reasoning) = router.feed("<think>Visible answer");
        let (rest_content, rest_reasoning) = router.finish();

        assert_eq!(content, "");
        assert_eq!(reasoning, "");
        assert_eq!(rest_content, "Visible answer");
        assert_eq!(rest_reasoning, "Visible answer");
    }

    #[test]
    fn tool_call_router_hides_xml_from_visible_content() {
        let mut router = ToolCallContentRouter::default();

        let first = router.feed("Before <tool_call>{\"name\":\"datetime\"}");
        let second = router.feed("{\"arguments\":{}}</tool_call> after");
        let rest = router.finish();

        assert_eq!(first, "Before ");
        assert_eq!(second, " after");
        assert_eq!(rest, "");
    }

    #[test]
    fn tool_call_router_keeps_plain_text_streaming() {
        let mut router = ToolCallContentRouter::default();

        let first = router.feed("Halo ");
        let second = router.feed("hermes!");
        let rest = router.finish();

        assert_eq!(first, "Halo ");
        assert_eq!(second, "hermes!");
        assert_eq!(rest, "");
    }

    #[test]
    fn extract_tool_calls_from_choice_handles_non_streaming_calls() {
        let tool_calls = extract_tool_calls_from_choice(Some(vec![crate::client::ToolCallDelta {
            index: 0,
            id: Some("call_1".to_string()),
            call_type: Some("function".to_string()),
            function: Some(crate::client::ToolCallFunction {
                name: "datetime".to_string(),
                arguments: "{\"timezone\":\"UTC\"}".to_string(),
            }),
        }]));

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].function.name, "datetime");
    }

    #[test]
    fn extract_tool_calls_from_choice_ignores_empty_entries() {
        let tool_calls = extract_tool_calls_from_choice(Some(vec![crate::client::ToolCallDelta {
            index: 0,
            id: None,
            call_type: None,
            function: None,
        }]));

        assert!(tool_calls.is_empty());
    }

    #[test]
    fn merge_stream_tool_call_appends_incremental_arguments() {
        let mut tool_calls = vec![ToolCall {
            id: "call_0_datetime".to_string(),
            function: crate::client::ToolCallFunction {
                name: "datetime".to_string(),
                arguments: "{\"format\":".to_string(),
            },
        }];

        merge_stream_tool_call(
            &mut tool_calls,
            ToolCall {
                id: "call_0_datetime".to_string(),
                function: crate::client::ToolCallFunction {
                    name: "datetime".to_string(),
                    arguments: "\"%Y-%m-%d\"}".to_string(),
                },
            },
        );

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(
            tool_calls[0].function.arguments,
            "{\"format\":\"%Y-%m-%d\"}"
        );
    }

    #[test]
    fn tool_call_router_hides_split_tool_call_open_tag() {
        let mut router = ToolCallContentRouter::default();

        let first = router.feed("Before <tool_ca");
        let second = router.feed("ll>{\"name\":\"datetime\"}</tool_call> after");
        let rest = router.finish();

        assert_eq!(first, "Before ");
        assert_eq!(second, " after");
        assert_eq!(rest, "");
    }

    #[tokio::test]
    async fn process_response_parses_xml_tool_calls_in_non_stream_mode() {
        let agent = HermesAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );

        let response = ChatResponse {
            id: "resp_1".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "demo".to_string(),
            choices: vec![crate::client::Choice {
                index: 0,
                message: crate::client::MessageDelta {
                    role: Some(crate::client::Role::Assistant),
                    content: Some(
                        "<tool_call>{\"name\":\"datetime\",\"arguments\":\"{}\"}</tool_call>"
                            .to_string(),
                    ),
                    reasoning_content: Some("need tool".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: crate::client::Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            },
        };

        let telemetry = AgentTelemetry {
            prompt_tokens: 1,
            completion_tokens: 0,
            total_tokens: 1,
            context_window: 128_000,
            compacted: false,
            estimated: true,
            billable: false,
        };
        let (content, reasoning, tool_calls) =
            agent.process_response(response, &telemetry).await.unwrap();

        assert_eq!(content, "");
        assert_eq!(reasoning, "need tool");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "datetime");
    }

    #[tokio::test]
    async fn request_messages_auto_compact_when_context_is_full() {
        let config = AgentConfig {
            context_window: 20,
            ..AgentConfig::default()
        };
        let agent = HermesAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent
            .user_message("first long message that should be compacted away")
            .await;
        agent
            .user_message("second long message that should remain newest")
            .await;

        let messages = agent.build_messages().await.unwrap();
        let (request_messages, telemetry) = agent.prepare_request_messages(&messages, &[]).unwrap();

        assert!(telemetry.compacted);
        assert!(request_messages.len() < messages.len());
        assert!(request_messages
            .iter()
            .any(|message| message.content.contains("second long message")));
    }

    #[tokio::test]
    async fn request_messages_count_tool_schema_tokens() {
        let config = AgentConfig {
            context_window: 80,
            ..AgentConfig::default()
        };
        let agent = HermesAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = vec![
            Message::system("system"),
            Message::user("older message that can be compacted"),
            Message::user("latest request"),
        ];
        let tools = vec![ToolSchema::new(
            "large_tool",
            "large tool description".repeat(20),
            serde_json::json!({ "type": "object", "properties": { "query": { "type": "string" } } }),
        )];

        let (request_messages, telemetry) =
            agent.prepare_request_messages(&messages, &tools).unwrap();

        assert!(telemetry.compacted);
        assert!(telemetry.prompt_tokens >= total_tool_schema_tokens(&tools));
        assert!(request_messages
            .iter()
            .any(|message| message.content == "latest request"));
    }

    #[tokio::test]
    async fn streaming_emits_incremental_telemetry() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let agent = HermesAgent::with_events(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
            tx,
        );
        let chunks: Vec<std::result::Result<bytes::Bytes, reqwest::Error>> = vec![
            Ok(bytes::Bytes::from_static(
                b"data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"demo\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello world, this is a longer streamed chunk. \"},\"finish_reason\":null}]}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"demo\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Second streamed chunk for telemetry.\"},\"finish_reason\":null}]}\n\n",
            )),
        ];
        let stream = ChatStreamResponse::new(futures::stream::iter(chunks));
        let preflight = AgentTelemetry {
            prompt_tokens: 10,
            completion_tokens: 0,
            total_tokens: 10,
            context_window: 100,
            compacted: false,
            estimated: true,
            billable: false,
        };

        let (content, _, _) = agent.process_stream(stream, &preflight).await.unwrap();

        assert!(content.contains("Second streamed chunk"));
        let mut telemetry_events = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, AgentEvent::Telemetry { .. }) {
                telemetry_events += 1;
            }
        }
        assert!(telemetry_events >= 2);
    }

    #[tokio::test]
    async fn streaming_telemetry_counts_native_tool_calls() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let agent = HermesAgent::with_events(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
            tx,
        );
        let chunks: Vec<std::result::Result<bytes::Bytes, reqwest::Error>> = vec![Ok(
            bytes::Bytes::from_static(
                b"data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"demo\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"datetime\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
            ),
        )];
        let stream = ChatStreamResponse::new(futures::stream::iter(chunks));
        let preflight = AgentTelemetry {
            prompt_tokens: 10,
            completion_tokens: 0,
            total_tokens: 10,
            context_window: 100,
            compacted: false,
            estimated: true,
            billable: false,
        };

        let (_, _, tool_calls) = agent.process_stream(stream, &preflight).await.unwrap();

        assert_eq!(tool_calls.len(), 1);
        let mut max_completion_tokens = 0;
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Telemetry { telemetry } = event {
                max_completion_tokens = max_completion_tokens.max(telemetry.completion_tokens);
            }
        }
        assert!(max_completion_tokens > 0);
    }

    #[tokio::test]
    async fn process_stream_handles_anthropic_style_events() {
        let agent = HermesAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let chunks: Vec<std::result::Result<bytes::Bytes, reqwest::Error>> = vec![
            Ok(bytes::Bytes::from_static(
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"checking\"}}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from Claude\"}}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"datetime\",\"input\":{}}}\n\n",
            )),
            Ok(bytes::Bytes::from_static(
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            )),
        ];
        let stream = ChatStreamResponse::new(futures::stream::iter(chunks));
        let preflight = AgentTelemetry {
            prompt_tokens: 10,
            completion_tokens: 0,
            total_tokens: 10,
            context_window: 100,
            compacted: false,
            estimated: true,
            billable: false,
        };

        let (content, reasoning, tool_calls) =
            agent.process_stream(stream, &preflight).await.unwrap();

        assert_eq!(content, "Hello from Claude");
        assert_eq!(reasoning, "checking");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "toolu_1");
        assert_eq!(tool_calls[0].function.name, "datetime");
        assert_eq!(tool_calls[0].function.arguments, "{}");
    }

    #[tokio::test]
    async fn process_stream_keeps_empty_anthropic_tool_input_valid_json() {
        let agent = HermesAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let chunks: Vec<std::result::Result<bytes::Bytes, reqwest::Error>> = vec![Ok(
            bytes::Bytes::from_static(
                b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_empty\",\"name\":\"datetime\",\"input\":{}}}\n\n",
            ),
        )];
        let stream = ChatStreamResponse::new(futures::stream::iter(chunks));
        let preflight = AgentTelemetry {
            prompt_tokens: 10,
            completion_tokens: 0,
            total_tokens: 10,
            context_window: 100,
            compacted: false,
            estimated: true,
            billable: false,
        };

        let (_, _, tool_calls) = agent.process_stream(stream, &preflight).await.unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.arguments, "{}");
    }

    #[test]
    fn compact_request_messages_keeps_oversized_latest_user_request() {
        let messages = vec![
            Message::system("system"),
            Message::user("older context that can be dropped"),
            Message::user("latest oversized user request that must still reach the model"),
        ];

        let compacted = compact_request_messages(&messages, 4);

        assert!(compacted
            .iter()
            .any(|message| message.content.contains("latest oversized")));
    }

    #[test]
    fn compact_request_messages_preserves_tool_call_group() {
        let assistant = Message::assistant("calling tool").with_tool_calls(vec![ToolCall {
            id: "call_1".to_string(),
            function: crate::client::ToolCallFunction {
                name: "datetime".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        let messages = vec![
            Message::system("system"),
            Message::user("latest user request that caused the tool call"),
            assistant,
            Message::tool("call_1", "tool result"),
        ];

        let compacted = compact_request_messages(&messages, 4);

        let tool_index = compacted
            .iter()
            .position(|message| message.role == crate::client::Role::Tool)
            .expect("tool result should be preserved");
        assert!(compacted
            .iter()
            .any(|message| message.content.contains("latest user request")));
        assert!(tool_index > 0);
        assert_eq!(
            compacted[tool_index - 1].role,
            crate::client::Role::Assistant
        );
        assert!(compacted[tool_index - 1].tool_calls.is_some());
    }

    #[tokio::test]
    async fn execute_tools_preserves_model_tool_call_id() {
        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry.register(WrongIdTool).await.unwrap();
        let agent = HermesAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );

        let results = agent
            .execute_tools(vec![ToolCall {
                id: "call_from_model".to_string(),
                function: crate::client::ToolCallFunction {
                    name: "wrong_id".to_string(),
                    arguments: "{}".to_string(),
                },
            }])
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].tool_call_id, "call_from_model");
    }

    #[tokio::test]
    async fn execute_tools_returns_model_id_for_invalid_json() {
        let agent = HermesAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );

        let results = agent
            .execute_tools(vec![ToolCall {
                id: "bad_json_call".to_string(),
                function: crate::client::ToolCallFunction {
                    name: "wrong_id".to_string(),
                    arguments: "{".to_string(),
                },
            }])
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert_eq!(results[0].tool_call_id, "bad_json_call");
        assert!(results[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Invalid JSON"));
    }
}
