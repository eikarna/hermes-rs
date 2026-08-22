//! Kerux Agent orchestration loop with self-healing
//!
//! Implements the ReAct (Reason + Act) pattern for LLM-driven tool execution.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, RwLock};
use tokio::time::timeout;
use tracing::{debug, error, info, instrument, warn};

#[cfg(test)]
use crate::client::AnthropicClient;
use crate::client::{
    ChatResponse, ChatStreamEvent, ChatStreamResponse, LLMProvider, Message, OpenAIClient, Role,
    ToolCall,
};
use crate::config::{runtime_config, BehaviorSettings};
use crate::context::{estimate_message_tokens, estimate_tokens};
use crate::context_files::{load_default_context_files, load_workspace_context};
use crate::error::{Error, Result};
use crate::memory::MemoryManager;
use crate::parser::{ToolCallParser, ToolCallStreamParser};
use crate::schema::ToolSchema;
use crate::tools::{ToolContext, ToolRegistry, ToolResult};

/// Prefix marking the rolling context-summary system message that
/// [`KeruxAgent::compact_history`] embeds as the first conversation entry.
pub const CONTEXT_SUMMARY_MARKER: &str = "[CONTEXT SUMMARY]";

/// Configuration for the Kerux agent
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
    /// Token budget for `<repo_map>` injection; `0` disables it.
    pub repo_map_tokens: usize,
    /// Maximum files discovered for repo map scoring (cap huge repos).
    pub repo_map_max_files: usize,
    /// Override the capability-table edit format hint. `None` = table behavior.
    pub edit_format_override: Option<crate::client::EditFormat>,
    /// Task 2.3 bounded repair policy: per-path edit repair budget. `None`
    /// falls back to `max_healing_attempts`.
    pub max_repair_attempts: Option<usize>,
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
            repo_map_tokens: settings.repo_map_tokens,
            repo_map_max_files: settings.repo_map_max_files,
            edit_format_override: settings.edit_format_override,
            max_repair_attempts: settings.max_repair_attempts,
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
    ToolStart {
        /// Tool call ID (correlates with ToolComplete's result.tool_call_id)
        call_id: String,
        name: String,
        arguments: String,
    },
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

fn unix_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn agent_event_record(event: &AgentEvent) -> Option<(&'static str, serde_json::Value)> {
    match event {
        AgentEvent::ToolStart {
            call_id,
            name,
            arguments,
        } => Some((
            "tool_started",
            serde_json::json!({
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
            }),
        )),
        AgentEvent::ToolComplete { result } => Some((
            "tool_completed",
            serde_json::json!({
                "call_id": result.tool_call_id,
                "success": result.success,
                "content": result.content,
            }),
        )),
        AgentEvent::Reasoning { text } => Some((
            "reasoning_metadata",
            serde_json::json!({
                "bytes": text.len(),
                "sha256": format!("{:x}", Sha256::digest(text.as_bytes())),
            }),
        )),
        AgentEvent::Thinking { content } => Some((
            "thinking_metadata",
            serde_json::json!({
                "bytes": content.len(),
                "sha256": format!("{:x}", Sha256::digest(content.as_bytes())),
            }),
        )),
        AgentEvent::Content { text } => {
            Some(("content_delta", serde_json::json!({"content": text})))
        }
        AgentEvent::Done { message } => Some((
            "assistant_message",
            serde_json::json!({
                "content": message.content,
                "tool_call_ids": message
                    .tool_calls
                    .as_ref()
                    .map(|calls| calls.iter().map(|call| call.id.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            }),
        )),
        AgentEvent::Telemetry { telemetry } => Some((
            "telemetry",
            serde_json::json!({
                "prompt_tokens": telemetry.prompt_tokens,
                "completion_tokens": telemetry.completion_tokens,
                "total_tokens": telemetry.total_tokens,
                "context_window": telemetry.context_window,
                "compacted": telemetry.compacted,
                "estimated": telemetry.estimated,
                "billable": telemetry.billable,
            }),
        )),
        AgentEvent::IterationComplete { iteration } => Some((
            "iteration_completed",
            serde_json::json!({"iteration": iteration}),
        )),
        AgentEvent::ToolError { name, error } => Some((
            "tool_failed",
            serde_json::json!({"call_id": name, "error": error}),
        )),
        AgentEvent::Error { .. } => None,
    }
}

/// Kerux Agent for tool orchestration
pub struct KeruxAgent {
    config: AgentConfig,
    client: Arc<dyn LLMProvider>,
    registry: ToolRegistry,
    conversation: Arc<RwLock<Vec<Message>>>,
    /// Event sender. Wrapped in a Mutex so gateway runs can swap the sink
    /// per-run (one shared agent, many sequential chat turns).
    event_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<AgentEvent>>>>,
    /// Optional per-run recorder, parallel to the UI/gateway event sink.
    run_recorder: Arc<std::sync::Mutex<Option<Arc<crate::run_journal::RunRecorder>>>>,
    /// Cooperative cancellation flag. Set externally (e.g. by the gateway on
    /// user interrupt); checked between iterations, per stream chunk, and
    /// before each tool execution.
    cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    memory_manager: Option<MemoryManager>,
    /// Lazily-rendered `<repo_map>` block; parsed once per agent so per-turn
    /// message rebuilds stay cheap.
    repo_map_cache: Arc<tokio::sync::OnceCell<String>>,
    /// Optional human-approval gate consulted before dangerous tools run.
    /// Swapped per-run by the gateway (like `event_tx`).
    approval_gate: Arc<std::sync::Mutex<Option<Arc<dyn crate::approval::ToolApprovalGate>>>>,
    /// Per-run edit-protocol outcome tracker (Task 2.4 measurement).
    edit_metrics: Arc<std::sync::Mutex<crate::edit_metrics::EditMetricsTracker>>,
}

impl KeruxAgent {
    /// Per-path edit repair budget (Task 2.3): `behavior.max_repair_attempts`
    /// when configured, otherwise the healing-attempt bound.
    fn repair_budget_from(config: &AgentConfig) -> u32 {
        config
            .max_repair_attempts
            .unwrap_or(config.max_healing_attempts)
            .min(u32::MAX as usize) as u32
    }

    /// Create a new Kerux agent
    pub fn new(config: AgentConfig, client: OpenAIClient, registry: ToolRegistry) -> Self {
        Self::new_with_provider(config, Arc::new(client), registry)
    }

    /// Create a new Kerux agent with any configured LLM provider.
    pub fn new_with_provider(
        config: AgentConfig,
        client: Arc<dyn LLMProvider>,
        registry: ToolRegistry,
    ) -> Self {
        let edit_tracker = crate::edit_metrics::EditMetricsTracker::with_repair_budget(
            Self::repair_budget_from(&config),
        );
        Self {
            config,
            client,
            registry,
            conversation: Arc::new(RwLock::new(Vec::new())),
            event_tx: Arc::new(std::sync::Mutex::new(None)),
            run_recorder: Arc::new(std::sync::Mutex::new(None)),
            cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            memory_manager: None,
            repo_map_cache: Arc::new(tokio::sync::OnceCell::new()),
            approval_gate: Arc::new(std::sync::Mutex::new(None)),
            edit_metrics: Arc::new(std::sync::Mutex::new(edit_tracker)),
        }
    }

    /// Create with event channel for streaming events
    pub fn with_events(
        config: AgentConfig,
        client: OpenAIClient,
        registry: ToolRegistry,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Self {
        Self::with_provider_events(config, Arc::new(client), registry, event_tx)
    }

    /// Create with an event channel and any configured LLM provider.
    pub fn with_provider_events(
        config: AgentConfig,
        client: Arc<dyn LLMProvider>,
        registry: ToolRegistry,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Self {
        let edit_tracker = crate::edit_metrics::EditMetricsTracker::with_repair_budget(
            Self::repair_budget_from(&config),
        );
        Self {
            config,
            client,
            registry,
            conversation: Arc::new(RwLock::new(Vec::new())),
            event_tx: Arc::new(std::sync::Mutex::new(Some(event_tx))),
            run_recorder: Arc::new(std::sync::Mutex::new(None)),
            cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            memory_manager: None,
            repo_map_cache: Arc::new(tokio::sync::OnceCell::new()),
            approval_gate: Arc::new(std::sync::Mutex::new(None)),
            edit_metrics: Arc::new(std::sync::Mutex::new(edit_tracker)),
        }
    }

    /// Swap the event sink for the next run. Lets a long-lived shared agent
    /// stream events to a fresh per-run consumer (e.g. a gateway turn).
    pub fn set_event_sender(&self, sender: Option<mpsc::Sender<AgentEvent>>) {
        if let Ok(mut guard) = self.event_tx.lock() {
            *guard = sender;
        }
    }

    /// Swap the native run recorder without changing surface event delivery.
    pub fn set_run_recorder(&self, recorder: Option<Arc<crate::run_journal::RunRecorder>>) {
        if let Ok(mut guard) = self.run_recorder.lock() {
            *guard = recorder;
        }
    }

    /// Swap the approval gate for the next run. `None` disables approval
    /// prompts (tools run immediately).
    pub fn set_approval_gate(&self, gate: Option<Arc<dyn crate::approval::ToolApprovalGate>>) {
        if let Ok(mut guard) = self.approval_gate.lock() {
            *guard = gate;
        }
    }

    /// Get a handle to the cancellation flag. Setting it to `true` stops the
    /// current run at the next checkpoint (iteration boundary, stream chunk,
    /// or tool start).
    pub fn cancel_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.cancel_flag.clone()
    }

    /// Request cancellation of the current run.
    pub fn cancel(&self) {
        self.cancel_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Attach a memory manager for long-term memory injection and session distillation.
    pub fn with_memory_manager(mut self, memory_manager: MemoryManager) -> Self {
        self.memory_manager = Some(memory_manager);
        self
    }

    /// Send an event to the surface channel and optional native recorder.
    async fn emit(&self, event: AgentEvent) -> Result<()> {
        let tx = { self.event_tx.lock().ok().and_then(|guard| guard.clone()) };
        if let Some(tx) = tx {
            // Non-blocking send: progress events are decorative. If the
            // consumer (progress pump) is dead or wedged, the bounded
            // channel fills up — blocking here would deadlock the entire
            // agent loop. Drop the event instead.
            let _ = tx.try_send(event.clone());
        }
        self.record_agent_event(&event)?;
        Ok(())
    }

    fn record_agent_event(&self, event: &AgentEvent) -> Result<()> {
        let recorder = self
            .run_recorder
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let Some(recorder) = recorder else {
            return Ok(());
        };

        let Some((kind, payload)) = agent_event_record(event) else {
            return Ok(());
        };
        if let Err(error) = recorder.record(unix_timestamp_ms(), kind, payload) {
            match recorder.failure_mode() {
                crate::run_journal::RecorderFailureMode::Warn => {
                    warn!(error = %error, "Run recorder failed; continuing in warn mode");
                }
                crate::run_journal::RecorderFailureMode::Fail => {
                    return Err(Error::Agent(format!("run recorder failed: {error}")));
                }
            }
        }
        Ok(())
    }

    /// Record a `request_prepared` provenance event capturing the exact
    /// inputs assembled for an LLM request: message digests, context files,
    /// memory blocks, tool schemas, and provider capabilities.
    async fn record_request_prepared(
        &self,
        iteration: usize,
        request_messages: &[Message],
        tools: &[ToolSchema],
        telemetry: &AgentTelemetry,
    ) -> Result<()> {
        let recorder = self
            .run_recorder
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let Some(recorder) = recorder else {
            return Ok(());
        };

        // Per-message digests: role + sha256 of content + token estimate.
        let message_digests: Vec<serde_json::Value> = request_messages
            .iter()
            .map(|msg| {
                serde_json::json!({
                    "role": format!("{:?}", msg.role),
                    "sha256": format!("{:x}", Sha256::digest(msg.content.as_bytes())),
                    "tokens": estimate_tokens(&msg.content),
                })
            })
            .collect();

        // Context file provenance (default + workspace).
        let mut context_files: Vec<serde_json::Value> = Vec::new();
        for meta in crate::context_files::default_context_file_metas() {
            context_files.push(serde_json::to_value(&meta).unwrap_or_default());
        }
        if let Ok(cwd) = std::env::current_dir() {
            for meta in crate::context_files::workspace_context_file_metas(&cwd) {
                context_files.push(serde_json::to_value(&meta).unwrap_or_default());
            }
        }

        // Memory blocks injected into context (importance >= 70).
        let mut memory_blocks: Vec<serde_json::Value> = Vec::new();
        if let Some(memory) = &self.memory_manager {
            for block in memory.get_important(70).await {
                memory_blocks.push(serde_json::json!({
                    "id": block.id,
                    "block_type": block.block_type,
                    "importance": block.importance,
                    "sha256": format!("{:x}", Sha256::digest(block.content.as_bytes())),
                }));
            }
        }

        // Tool schema digests.
        let tool_schemas: Vec<serde_json::Value> = tools
            .iter()
            .map(|tool| {
                let schema_json = serde_json::to_string(tool).unwrap_or_default();
                serde_json::json!({
                    "name": tool.name,
                    "sha256": format!("{:x}", Sha256::digest(schema_json.as_bytes())),
                })
            })
            .collect();

        // Provider capabilities for the active model.
        let caps = self.client.capabilities(&self.config.model);
        let capabilities = serde_json::json!({
            "max_input_tokens": caps.max_input_tokens,
            "max_output_tokens": caps.max_output_tokens,
            "edit_format": format!("{:?}", caps.edit_format),
            "supports_streaming": caps.supports_streaming,
            "supports_reasoning": caps.supports_reasoning,
            "supports_vision": caps.supports_vision,
            "supports_tool_calls": caps.supports_tool_calls,
        });

        let payload = serde_json::json!({
            "iteration": iteration,
            "model": self.config.model,
            "message_count": request_messages.len(),
            "message_digests": message_digests,
            "context_files": context_files,
            "memory_blocks": memory_blocks,
            "tool_schemas": tool_schemas,
            "capabilities": capabilities,
            "skills": [],
            "telemetry": {
                "prompt_tokens": telemetry.prompt_tokens,
                "context_window": telemetry.context_window,
                "compacted": telemetry.compacted,
            },
        });

        if let Err(error) = recorder.record(unix_timestamp_ms(), "request_prepared", payload) {
            match recorder.failure_mode() {
                crate::run_journal::RecorderFailureMode::Warn => {
                    warn!(error = %error, "Run recorder failed; continuing in warn mode");
                }
                crate::run_journal::RecorderFailureMode::Fail => {
                    return Err(Error::Agent(format!("run recorder failed: {error}")));
                }
            }
        }
        Ok(())
    }

    /// Record an `approval_decision` event so every approval request and its
    /// outcome is auditable and correlated with the tool call it gates.
    ///
    /// Approval is a human decision channel — it is never labelled as
    /// sandbox enforcement. The denial reason is redacted by the journal's
    /// central redaction pass before it is persisted.
    fn record_approval_decision(
        &self,
        call_id: &str,
        tool_name: &str,
        decision: &crate::approval::ApprovalDecision,
    ) -> Result<()> {
        let recorder = self
            .run_recorder
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let Some(recorder) = recorder else {
            return Ok(());
        };

        let (approved, outcome, reason) = match decision {
            crate::approval::ApprovalDecision::Approved => (true, "approved", None),
            crate::approval::ApprovalDecision::Denied { reason, outcome } => (
                false,
                match outcome {
                    crate::approval::ApprovalOutcome::Approved => "approved",
                    crate::approval::ApprovalOutcome::Denied => "denied",
                    crate::approval::ApprovalOutcome::Timeout => "timeout",
                    crate::approval::ApprovalOutcome::ChannelClosed => "channel_closed",
                    crate::approval::ApprovalOutcome::PromptFailed => "prompt_failed",
                },
                Some(reason.as_str()),
            ),
        };

        let payload = serde_json::json!({
            "call_id": call_id,
            "tool_name": tool_name,
            "approved": approved,
            "outcome": outcome,
            "reason": reason,
        });

        if let Err(error) = recorder.record(unix_timestamp_ms(), "approval_decision", payload) {
            match recorder.failure_mode() {
                crate::run_journal::RecorderFailureMode::Warn => {
                    warn!(error = %error, "Run recorder failed; continuing in warn mode");
                }
                crate::run_journal::RecorderFailureMode::Fail => {
                    return Err(Error::Agent(format!("run recorder failed: {error}")));
                }
            }
        }
        Ok(())
    }

    /// Journal one edit-protocol outcome event (Task 2.4). Measurement only:
    /// never alters tool selection or execution.
    ///
    /// Called from [`Self::execute_tools`] after every edit-format tool call
    /// reaches a terminal state. When no recorder is attached this is a no-op
    /// apart from tracker bookkeeping.
    fn record_edit_outcome(
        &self,
        call_id: &str,
        tool_name: &str,
        args_str: &str,
        parse_status: &'static str,
        outcome: crate::edit_metrics::EditApplyStatus,
        result_content: Option<&str>,
    ) -> Result<()> {
        use crate::edit_metrics::{
            extract_target_path, language_for_path, match_type_from_payload, EditFormat,
        };

        let format = match EditFormat::from_tool_name(tool_name) {
            Some(f) => f,
            None => return Ok(()),
        };
        let path = extract_target_path(args_str);
        // Repair-relevant failure: an apply error, or a parse failure that
        // stopped the edit before execution. Cancels/denials are not.
        let failed = matches!(outcome, crate::edit_metrics::EditApplyStatus::Failed)
            || (matches!(outcome, crate::edit_metrics::EditApplyStatus::Skipped)
                && parse_status == "failed");
        // Task 2.5: a *classified* edit-application failure promotes the
        // one-way static fallback ladder so the next iteration prompts for a
        // stronger protocol. Measurement stays passive; only routing moves.
        if failed {
            if let Some(failed_path) = path.as_deref() {
                self.edit_metrics
                    .lock()
                    .map(|mut t| t.record_fallback(failed_path, format))
                    .ok();
            }
        }
        let (repair_count, pass_kind, repair_allowed) = self
            .edit_metrics
            .lock()
            .map(|mut t| t.observe(path.as_deref().unwrap_or(""), failed))
            .unwrap_or((0, crate::edit_metrics::EditPassKind::FirstPass, true));
        let run_attempt = self
            .edit_metrics
            .lock()
            .map(|t| t.run_attempt())
            .unwrap_or(1);

        let recorder = self
            .run_recorder
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let Some(recorder) = recorder else {
            return Ok(());
        };

        // Parse status comes straight from the call site: only the invalid-
        // JSON skip reports "failed"; cancels/denials/unknown tools either
        // parsed fine or were never attempted, which callers state exactly.

        let payload = serde_json::json!({
            "call_id": call_id,
            "tool_name": tool_name,
            "format": format.as_str(),
            "effective_format": self
                .edit_metrics
                .lock()
                .ok()
                .and_then(|t| t.format_hint())
                .map(|h| h.as_str()),
            "path": path,
            "parse_status": parse_status,
            "apply_status": outcome,
            "match_type": match (&outcome, result_content) {
                (crate::edit_metrics::EditApplyStatus::Ok, Some(content)) => {
                    match_type_from_payload(content)
                }
                _ => None,
            },
            "language": path.as_deref().map(language_for_path),
            "repair_count": repair_count,
            "run_attempt": run_attempt,
            "pass_kind": pass_kind.as_str(),
            "repair_allowed": repair_allowed,
            "provider_kind": recorder.provider_kind().ok(),
            "model": recorder.model().ok(),
        });

        if let Err(error) = recorder.record(unix_timestamp_ms(), "edit_outcome", payload) {
            match recorder.failure_mode() {
                crate::run_journal::RecorderFailureMode::Warn => {
                    warn!(error = %error, "Run recorder failed; continuing in warn mode");
                }
                crate::run_journal::RecorderFailureMode::Fail => {
                    return Err(Error::Agent(format!("run recorder failed: {error}")));
                }
            }
        }
        Ok(())
    }

    fn finish_recording(
        &self,
        status: crate::run_journal::RunStatus,
        kind: &'static str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let recorder = self
            .run_recorder
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let Some(recorder) = recorder else {
            return Ok(());
        };

        if let Err(error) = recorder.finish(unix_timestamp_ms(), kind, payload, status) {
            match recorder.failure_mode() {
                crate::run_journal::RecorderFailureMode::Warn => {
                    warn!(error = %error, "Run recorder finalization failed; continuing in warn mode");
                }
                crate::run_journal::RecorderFailureMode::Fail => {
                    return Err(Error::Agent(format!(
                        "run recorder finalization failed: {error}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn finish_run_result(&self, result: &Result<Message>) -> Result<()> {
        match result {
            Ok(_) => self.finish_recording(
                crate::run_journal::RunStatus::Succeeded,
                "run_completed",
                serde_json::json!({}),
            ),
            Err(Error::Cancelled) => self.finish_recording(
                crate::run_journal::RunStatus::Cancelled,
                "run_cancelled",
                serde_json::json!({"reason": "user_cancelled"}),
            ),
            Err(error) => self.finish_recording(
                crate::run_journal::RunStatus::Failed,
                "run_failed",
                serde_json::json!({"error": error.to_string()}),
            ),
        }
    }

    fn start_recording(&self) -> Result<()> {
        // Fresh run: forget per-path repair counters from any previous run so
        // `repair_count` never leaks across runs.
        if let Ok(mut tracker) = self.edit_metrics.lock() {
            tracker.reset();
        }

        let recorder = self
            .run_recorder
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let Some(recorder) = recorder else {
            return Ok(());
        };

        if let Err(error) =
            recorder.record(unix_timestamp_ms(), "run_started", serde_json::json!({}))
        {
            match recorder.failure_mode() {
                crate::run_journal::RecorderFailureMode::Warn => {
                    warn!(error = %error, "Run recorder start failed; continuing in warn mode");
                }
                crate::run_journal::RecorderFailureMode::Fail => {
                    return Err(Error::Agent(format!("run recorder start failed: {error}")));
                }
            }
        }

        // Attach git checkpoint evidence to the run manifest. Best-effort:
        // a missing git binary or a non-repo workspace just skips the
        // checkpoint rather than failing the run.
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(harness) = crate::githarness::GitHarness::open(&cwd) {
                match harness.checkpoint() {
                    Ok(checkpoint) => {
                        if let Err(error) = recorder.attach_git_checkpoint(&checkpoint) {
                            warn!(error = %error, "Failed to attach git checkpoint; continuing");
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "Failed to capture git checkpoint; continuing");
                    }
                }
            }
        }
        Ok(())
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

    /// Rolling context compaction.
    ///
    /// When the conversation buffer reaches `COMPACTION_TRIGGER` messages,
    /// the oldest ones are summarized in a single one-shot LLM call and
    /// replaced by one marker system message, keeping only the most recent
    /// `COMPACTION_KEEP` messages. A previous summary (from an earlier
    /// compaction) is rolled into the new one, so context degrades
    /// gracefully instead of being hard-truncated.
    ///
    /// Returns the new summary text, or `None` when nothing was compacted
    /// (buffer too small, no safe split point, or the summarization call
    /// failed — compaction is fail-open and never breaks a run).
    pub async fn compact_history(&self) -> Option<String> {
        /// Compact once the buffer reaches this many messages.
        const COMPACTION_TRIGGER: usize = 120;
        /// Keep this many most-recent messages verbatim.
        const COMPACTION_KEEP: usize = 40;
        /// Cap the transcript fed to the summarizer (~6k tokens).
        const TRANSCRIPT_CAP: usize = 24_000;

        let conv = self.conversation.read().await.clone();
        if conv.len() < COMPACTION_TRIGGER {
            return None;
        }

        // Split point: keep the last KEEP messages, but never start the
        // tail with an orphaned tool result, and never end the head with
        // an assistant message whose tool results would be discarded.
        let mut split = conv.len() - COMPACTION_KEEP;
        while split < conv.len() {
            let tail_starts_orphan = conv[split].role == Role::Tool;
            let head_ends_toolcall = split > 0
                && conv[split - 1].role == Role::Assistant
                && conv[split - 1].tool_calls.is_some();
            if !tail_starts_orphan && !head_ends_toolcall {
                break;
            }
            split += 1;
        }
        if split >= conv.len() {
            return None;
        }

        let (head, tail) = conv.split_at(split);

        // Roll the previous summary (if any) into the new one.
        let mut prior_summary = String::new();
        let mut transcript_start = 0;
        if let Some(first) = head.first() {
            if first.role == Role::System && first.content.starts_with(CONTEXT_SUMMARY_MARKER) {
                prior_summary = first
                    .content
                    .trim_start_matches(CONTEXT_SUMMARY_MARKER)
                    .trim()
                    .to_string();
                transcript_start = 1;
            }
        }

        // Render the discarded head as a compact transcript.
        let mut transcript = String::new();
        for m in &head[transcript_start..] {
            let line = match m.role {
                Role::User => format!("USER: {}\n", m.content),
                Role::Assistant => {
                    let mut line = String::new();
                    if let Some(calls) = &m.tool_calls {
                        let names: Vec<&str> =
                            calls.iter().map(|c| c.function.name.as_str()).collect();
                        line.push_str(&format!(
                            "ASSISTANT: [called tools: {}]\n",
                            names.join(", ")
                        ));
                    }
                    if !m.content.trim().is_empty() {
                        line.push_str(&format!("ASSISTANT: {}\n", m.content));
                    }
                    line
                }
                Role::Tool => {
                    let preview: String = m.content.chars().take(200).collect();
                    format!("TOOL RESULT: {}\n", preview)
                }
                Role::System => String::new(),
            };
            if transcript.len() + line.len() > TRANSCRIPT_CAP {
                transcript.push_str("[transcript truncated]\n");
                break;
            }
            transcript.push_str(&line);
        }

        let mut prompt = String::from(
            "Summarize this conversation history so a future assistant can continue it \
             seamlessly. Preserve: user goals, decisions made, facts learned, file paths, \
             tool outcomes, and pending tasks. Be dense and factual; use bullet points; \
             no preamble.\n\n",
        );
        if !prior_summary.is_empty() {
            prompt.push_str("PREVIOUS SUMMARY (roll this into the new one):\n");
            prompt.push_str(&prior_summary);
            prompt.push_str("\n\n");
        }
        prompt.push_str("NEW TRANSCRIPT:\n");
        prompt.push_str(&transcript);

        let request = vec![Message::user(prompt)];
        let response = match self.client.chat(&self.config.model, &request, None).await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Context compaction summarization failed; keeping history as-is");
                return None;
            }
        };
        let summary_text = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;

        let mut new_conv = Vec::with_capacity(tail.len() + 1);
        new_conv.push(Message::system(format!(
            "{}\n{}",
            CONTEXT_SUMMARY_MARKER, summary_text
        )));
        new_conv.extend_from_slice(tail);
        *self.conversation.write().await = new_conv;

        info!(
            discarded = split - transcript_start,
            kept = tail.len(),
            "Compacted conversation history into rolling summary"
        );
        Some(summary_text)
    }

    /// The rolling context summary currently embedded in the conversation
    /// (first message carries the marker), if any.
    pub async fn context_summary(&self) -> Option<String> {
        let conv = self.conversation.read().await;
        conv.first()
            .filter(|m| m.role == Role::System && m.content.starts_with(CONTEXT_SUMMARY_MARKER))
            .map(|m| {
                m.content
                    .trim_start_matches(CONTEXT_SUMMARY_MARKER)
                    .trim()
                    .to_string()
            })
    }

    /// Run the agent with a user query
    #[instrument(skip(self), fields(model = % self.config.model))]
    pub async fn run(&self, user_query: String) -> Result<Message> {
        // Clear any stale cancellation from a previous run, then run with
        // the agent's own flag.
        self.cancel_flag
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.run_with_cancel(user_query, self.cancel_flag()).await
    }

    /// Run the agent with an external cancellation flag.
    ///
    /// The flag is checked at every iteration boundary, per streamed chunk,
    /// and before each tool execution. When it trips, the run stops and
    /// returns [`Error::Cancelled`] after repairing the conversation (any
    /// committed assistant tool_calls message gets tool results — real or
    /// placeholder — so the next request stays valid).
    #[instrument(skip(self, cancel), fields(model = % self.config.model))]
    pub async fn run_with_cancel(
        &self,
        user_query: String,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Message> {
        self.start_recording()?;
        let result = self.run_inner(user_query, cancel).await;
        self.finish_run_result(&result)?;
        result
    }

    async fn run_inner(
        &self,
        user_query: String,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Message> {
        info!("Starting agent run");

        // Add user message
        self.add_message(Message::user(&user_query)).await;

        // Build initial messages including system prompt
        let mut messages = self.build_messages().await?;
        let mut iteration = 0;

        loop {
            iteration += 1;
            debug!(iteration, "Agent iteration");

            // Cancellation checkpoint: between iterations.
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                info!("Agent run cancelled at iteration boundary");
                self.repair_conversation_after_cancel().await;
                return Err(Error::Cancelled);
            }

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
            .await?;

            // Get tool schemas
            let tools = self.registry.get_schemas().await;
            let (request_messages, preflight_telemetry) =
                self.prepare_request_messages(&messages, &tools)?;
            self.emit(AgentEvent::Telemetry {
                telemetry: preflight_telemetry.clone(),
            })
            .await?;
            self.record_request_prepared(
                iteration,
                &request_messages,
                &tools,
                &preflight_telemetry,
            )
            .await?;

            let response = if self.config.stream {
                let stream = self
                    .client
                    .chat_streaming(&self.config.model, &request_messages, Some(&tools))
                    .await?;
                match self
                    .process_stream(stream, &preflight_telemetry, &cancel)
                    .await
                {
                    Ok((response_text, reasoning_text, tool_calls)) => {
                        self.emit_stream_telemetry(
                            &preflight_telemetry,
                            &response_text,
                            &reasoning_text,
                            &tool_calls,
                        )
                        .await?;
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
                        .await?;
                        return Ok(result);
                    }

                    // Execute tools and add results
                    let tool_results = self.execute_tools(tool_calls, &cancel).await?;

                    for result in &tool_results {
                        if result.success {
                            self.emit(AgentEvent::ToolComplete {
                                result: result.clone(),
                            })
                            .await?;
                        } else {
                            self.emit(AgentEvent::ToolError {
                                name: result.tool_call_id.clone(),
                                error: result.error.clone().unwrap_or_default(),
                            })
                            .await?;
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
                    .await?;
                    return Err(e);
                }
            }

            self.emit(AgentEvent::IterationComplete { iteration })
                .await?;
        }
    }

    /// Build messages including system prompt
    async fn build_messages(&self) -> Result<Vec<Message>> {
        let mut messages = Vec::new();

        let mut system_prompt = if let Some(ref system) = self.config.system_prompt {
            system.clone()
        } else {
            "You are Kerux, an AI assistant that uses tools to help users. \
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

        if self.config.repo_map_tokens > 0 {
            let budget = self.config.repo_map_tokens;
            let max_files = self.config.repo_map_max_files;
            let rendered = self
                .repo_map_cache
                .get_or_init(|| async {
                    let root =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    // Tree-sitter parsing is blocking; move it off the async worker.
                    tokio::task::spawn_blocking(move || {
                        let map = crate::repomap::rank_and_render_with_limit(&root, &[], max_files);
                        crate::repomap::RepoMapRenderer::new(budget).render(&map)
                    })
                    .await
                    .unwrap_or_default()
                })
                .await;
            if rendered.trim() != "<repo_map>\n</repo_map>" && !rendered.trim().is_empty() {
                system_prompt.push_str("\n\n");
                system_prompt.push_str(rendered.trim_end());
                system_prompt.push('\n');
            }
        }

        // Task 2.5: routing precedence is explicit override > run fallback
        // hint (learned from this run's classified failures) > capability
        // table. The hint is one-way and never demotes mid-run.
        let edit_format = self
            .config
            .edit_format_override
            .or_else(|| {
                self.edit_metrics
                    .lock()
                    .ok()
                    .and_then(|t| t.format_hint())
                    .map(crate::edit_metrics::EditFormat::into_client)
            })
            .unwrap_or_else(|| self.client.capabilities(&self.config.model).edit_format);
        match edit_format {
            crate::client::EditFormat::SearchReplace => system_prompt.push_str(
                "\n\n<edit_format>\n\
                This model supports token-efficient search/replace edits. Prefer the \
                `edit_block` tool (ordered search/replace pairs, applied atomically) over \
                rewriting whole files with `file_write`.\n\
                </edit_format>",
            ),
            crate::client::EditFormat::Patch => system_prompt.push_str(
                "\n\n<edit_format>\n\
                This model prefers targeted patches. Use the `patch` tool (single exact \
                find-and-replace with fuzzy fallback) for edits instead of rewriting whole \
                files with `file_write`.\n\
                </edit_format>",
            ),
            crate::client::EditFormat::FullFile => {}
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
    ) -> Result<()> {
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
        .await
    }

    async fn emit_stream_telemetry_snapshot(
        &self,
        preflight: &AgentTelemetry,
        response_text: &str,
        reasoning_text: &str,
        tool_calls: &[ToolCall],
    ) -> Result<()> {
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
        .await
    }

    fn spawn_session_distillation(&self, history: Vec<Message>) {
        let Some(memory_manager) = self.memory_manager.clone() else {
            return;
        };

        let client = self.client.clone();
        let model = self.config.model.clone();
        tokio::spawn(async move {
            if let Err(error) = crate::distillation::distill_session_with_provider(
                client,
                model,
                memory_manager,
                history,
            )
            .await
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
        cancel: &Arc<std::sync::atomic::AtomicBool>,
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
            // Cancellation checkpoint: per streamed chunk. Drop the stream
            // and bail out; whatever text accumulated is discarded (the run
            // never commits a partial assistant message).
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                info!("Agent run cancelled mid-stream");
                drop(stream);
                return Err(Error::Cancelled);
            }
            match event_result {
                Ok(event) => {
                    // Process the event
                    if let Some(reasoning) = extract_reasoning_from_event(&event) {
                        let reasoning = strip_reasoning_tags(&reasoning);
                        if !reasoning.is_empty() {
                            accumulated_reasoning.push_str(&reasoning);
                            self.emit(AgentEvent::Reasoning { text: reasoning }).await?;
                            self.emit_stream_telemetry_snapshot(
                                preflight,
                                &accumulated_text,
                                &accumulated_reasoning,
                                &tool_calls,
                            )
                            .await?;
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
                                self.emit(AgentEvent::Content { text: visible_text })
                                    .await?;
                                self.emit_stream_telemetry_snapshot(
                                    preflight,
                                    &accumulated_text,
                                    &accumulated_reasoning,
                                    &tool_calls,
                                )
                                .await?;
                            }
                        }

                        if !reasoning_delta.is_empty() {
                            accumulated_reasoning.push_str(&reasoning_delta);
                            self.emit(AgentEvent::Reasoning {
                                text: reasoning_delta,
                            })
                            .await?;
                            self.emit_stream_telemetry_snapshot(
                                preflight,
                                &accumulated_text,
                                &accumulated_reasoning,
                                &tool_calls,
                            )
                            .await?;
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
                        .await?;
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
            .await?;
        }
        if !reasoning.is_empty() {
            self.emit(AgentEvent::Reasoning {
                text: reasoning.clone(),
            })
            .await?;
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
        .await?;

        Ok((content, reasoning, tool_calls))
    }

    /// Execute tools and handle self-healing
    async fn execute_tools(
        &self,
        tool_calls: Vec<ToolCall>,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Vec<ToolResult>> {
        let mut results = Vec::new();

        for tool_call in tool_calls {
            // Cancellation checkpoint: before each tool. Remaining tool calls
            // get placeholder results so the assistant message (already
            // committed to the conversation) stays paired with tool results.
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                info!(tool = %tool_call.function.name, "Agent run cancelled before tool execution");
                results.push(ToolResult::error(
                    &tool_call.id,
                    "Tool execution cancelled by user".to_string(),
                ));
                // Measurement only: a cancel is a skip, not a parse failure.
                let _ = self.record_edit_outcome(
                    &tool_call.id,
                    &tool_call.function.name,
                    &tool_call.function.arguments,
                    "cancelled",
                    crate::edit_metrics::EditApplyStatus::Skipped,
                    None,
                );
                continue;
            }

            let name = tool_call.function.name.clone();
            let args_str = tool_call.function.arguments.clone();

            debug!(tool = %name, args = %args_str, "Executing tool");
            self.emit(AgentEvent::ToolStart {
                call_id: tool_call.id.clone(),
                name: name.clone(),
                arguments: args_str.clone(),
            })
            .await?;

            // Parse arguments
            let args: serde_json::Value = match serde_json::from_str(&args_str) {
                Ok(a) => a,
                Err(e) => {
                    warn!(tool = %name, error = %e, "Failed to parse tool arguments");
                    results.push(ToolResult::error(
                        &tool_call.id,
                        format!("Invalid JSON: {}", e),
                    ));
                    let _ = self.record_edit_outcome(
                        &tool_call.id,
                        &name,
                        &args_str,
                        "failed",
                        crate::edit_metrics::EditApplyStatus::Skipped,
                        None,
                    );
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
                let _ = self.record_edit_outcome(
                    &tool_call.id,
                    &name,
                    &args_str,
                    "ok",
                    crate::edit_metrics::EditApplyStatus::Skipped,
                    None,
                );
                continue;
            }

            // Human approval gate for dangerous tools. The gate (installed
            // per-run by the gateway) presents the request and blocks until
            // the human decides or the gate's own timeout auto-denies.
            if crate::approval::requires_approval(&name) {
                let gate = self.approval_gate.lock().ok().and_then(|g| g.clone());
                if let Some(gate) = gate {
                    let preview: String = args_str.chars().take(300).collect();
                    let decision = gate
                        .request_approval(crate::approval::ApprovalRequest {
                            tool_name: name.clone(),
                            arguments_preview: preview,
                        })
                        .await;
                    self.record_approval_decision(&tool_call.id, &name, &decision)?;
                    if let crate::approval::ApprovalDecision::Denied { reason, .. } = decision {
                        info!(tool = %name, reason = %reason, "Tool execution denied by approval gate");
                        results.push(ToolResult::error(&tool_call.id, reason));
                        let _ = self.record_edit_outcome(
                            &tool_call.id,
                            &name,
                            &args_str,
                            "ok",
                            crate::edit_metrics::EditApplyStatus::Denied,
                            None,
                        );
                        continue;
                    }
                }
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
                    let _ = self.record_edit_outcome(
                        &tool_call.id,
                        &name,
                        &args_str,
                        "ok",
                        if r.success {
                            crate::edit_metrics::EditApplyStatus::Ok
                        } else {
                            crate::edit_metrics::EditApplyStatus::Failed
                        },
                        Some(&r.content),
                    );
                    results.push(r);
                }
                Ok(Err(e)) => {
                    error!(tool = %name, error = %e, "Tool execution failed");
                    let _ = self.record_edit_outcome(
                        &tool_call.id,
                        &name,
                        &args_str,
                        "ok",
                        crate::edit_metrics::EditApplyStatus::Failed,
                        None,
                    );
                    results.push(ToolResult::error(&tool_call.id, e.to_string()));
                }
                Err(_) => {
                    error!(tool = %name, "Tool execution timed out");
                    let _ = self.record_edit_outcome(
                        &tool_call.id,
                        &name,
                        &args_str,
                        "ok",
                        crate::edit_metrics::EditApplyStatus::Timeout,
                        None,
                    );
                    results.push(ToolResult::error(
                        &tool_call.id,
                        format!("Tool timed out after {:?}", self.config.tool_timeout),
                    ));
                }
            }
        }

        Ok(results)
    }

    /// Repair the conversation after a cancellation so the next request stays
    /// valid for the provider.
    ///
    /// If the last committed message is an assistant message carrying
    /// `tool_calls`, the provider expects a matching tool result for each call
    /// before any new turn. A cancel can leave that message dangling (the run
    /// stopped before tool results were appended). Append a `[cancelled]`
    /// placeholder result for every outstanding tool call so the history is
    /// well-formed.
    async fn repair_conversation_after_cancel(&self) {
        let mut conv = self.conversation.write().await;
        let needs_repair = conv
            .last()
            .map(|m| m.role == crate::client::Role::Assistant && m.tool_calls.is_some())
            .unwrap_or(false);

        if !needs_repair {
            return;
        }

        let tool_call_ids: Vec<String> = conv
            .last()
            .and_then(|m| m.tool_calls.clone())
            .map(|calls| calls.into_iter().map(|c| c.id).collect())
            .unwrap_or_default();

        let repaired = tool_call_ids.len();
        for id in tool_call_ids {
            conv.push(Message::tool(
                &id,
                "[cancelled] Tool execution was interrupted by the user.",
            ));
        }
        debug!(repaired, "Repaired conversation after cancel");
    }

    /// Run agent and handle self-healing on tool errors
    pub async fn run_with_healing(&self, user_query: String) -> Result<Message> {
        self.start_recording()?;
        self.cancel_flag
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let cancel = self.cancel_flag();
        let mut iteration = 0;
        let max_healing_attempts = self.config.max_healing_attempts;

        let result = loop {
            iteration += 1;
            // Task 2.3: tag the tracker with the top-level attempt identity so
            // every `edit_outcome` event reports which generation produced it
            // (1 = first pass, higher = evidence-fed repair attempt).
            if let Ok(mut tracker) = self.edit_metrics.lock() {
                tracker.set_run_attempt(iteration as u64);
            }

            match self.run_inner(user_query.clone(), cancel.clone()).await {
                Ok(response) => break Ok(response),
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
                    break Err(e);
                }
            }
        };

        self.finish_run_result(&result)?;
        result
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

/// Builder for creating a KeruxAgent
pub struct KeruxAgentBuilder {
    config: AgentConfig,
    client: Option<Arc<dyn LLMProvider>>,
    registry: Option<ToolRegistry>,
    memory_manager: Option<MemoryManager>,
}

impl KeruxAgentBuilder {
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
        self.client = Some(Arc::new(client));
        self
    }

    /// Set any configured LLM provider.
    pub fn provider(mut self, client: Arc<dyn LLMProvider>) -> Self {
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
    pub fn build(self) -> Result<KeruxAgent> {
        let client = match self.client {
            Some(client) => client,
            None => crate::client::build_provider_client(&runtime_config().client)?,
        };

        let registry = self
            .registry
            .unwrap_or_else(|| ToolRegistry::new(self.config.tool_timeout));

        let mut agent = KeruxAgent::new_with_provider(self.config, client, registry);
        if let Some(memory_manager) = self.memory_manager {
            agent = agent.with_memory_manager(memory_manager);
        }

        Ok(agent)
    }
}

impl Default for KeruxAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serial_test::serial;

    fn test_run_manifest(run_id: &str) -> crate::run_journal::RunManifestV1 {
        crate::run_journal::RunManifestV1 {
            schema_version: crate::run_journal::SCHEMA_VERSION,
            run_id: run_id.to_string(),
            parent_run_id: None,
            parent_sequence: None,
            created_at_ms: 1_725_000_000_000,
            completed_at_ms: None,
            status: crate::run_journal::RunStatus::Running,
            surface: "test".to_string(),
            model: "test-model".to_string(),
            provider_kind: "test-provider".to_string(),
            workspace_fingerprint: "test-workspace".to_string(),
            repository_head: None,
            repository_dirty_hash: None,
            repository_branch: None,
            repository_clean: None,
            repository_changed_files: vec![],
            recorder_policy: serde_json::json!({"max_payload_bytes": 1024}),
            last_sequence: None,
            last_hash: None,
            replayability: crate::run_journal::Replayability::Full,
            warnings: vec![],
        }
    }

    struct WrongIdTool;

    struct StaticProvider;

    #[async_trait]
    impl crate::client::LLMProvider for StaticProvider {
        async fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> Result<crate::client::ChatResponse> {
            Ok(crate::client::ChatResponse {
                id: "static-response".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "test-model".to_string(),
                choices: vec![crate::client::Choice {
                    index: 0,
                    message: crate::client::MessageDelta {
                        role: Some(crate::client::Role::Assistant),
                        content: Some("done".to_string()),
                        reasoning_content: None,
                        tool_calls: None,
                    },
                    finish_reason: Some("stop".to_string()),
                }],
                usage: crate::client::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                },
            })
        }

        async fn chat_streaming(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> Result<crate::client::ChatStreamResponse> {
            Err(Error::Agent("streaming not expected".to_string()))
        }

        fn capabilities(&self, _model: &str) -> crate::client::ProviderCapabilities {
            crate::client::ProviderCapabilities::default()
        }
    }

    struct FailingProvider;

    struct HealingProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl crate::client::LLMProvider for FailingProvider {
        async fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> Result<crate::client::ChatResponse> {
            Err(Error::Agent("provider exploded".to_string()))
        }

        async fn chat_streaming(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> Result<crate::client::ChatStreamResponse> {
            Err(Error::Agent("provider exploded".to_string()))
        }

        fn capabilities(&self, _model: &str) -> crate::client::ProviderCapabilities {
            crate::client::ProviderCapabilities::default()
        }
    }

    #[async_trait]
    impl crate::client::LLMProvider for HealingProvider {
        async fn chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> Result<crate::client::ChatResponse> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Err(Error::Agent("first attempt failed".to_string()));
            }
            StaticProvider.chat(_model, _messages, _tools).await
        }

        async fn chat_streaming(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: Option<&[crate::schema::ToolSchema]>,
        ) -> Result<crate::client::ChatStreamResponse> {
            Err(Error::Agent("streaming not expected".to_string()))
        }

        fn capabilities(&self, _model: &str) -> crate::client::ProviderCapabilities {
            crate::client::ProviderCapabilities::default()
        }
    }

    #[async_trait]
    impl crate::tools::KeruxTool for WrongIdTool {
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
        let _agent = KeruxAgentBuilder::new()
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
            "kerux_agent_auth_error_{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&auth_store_path);
        let old_auth_store = std::env::var("KERUX_AUTH_STORE").ok();
        let old_auth_ref = std::env::var("KERUX_AUTH_REF").ok();
        let old_api_key = std::env::var("OPENAI_API_KEY").ok();

        std::env::set_var("KERUX_AUTH_STORE", &auth_store_path);
        std::env::set_var("KERUX_AUTH_REF", "missing-profile");
        std::env::remove_var("OPENAI_API_KEY");

        let result = KeruxAgentBuilder::new().build();

        assert!(result.is_err());

        restore_env("KERUX_AUTH_STORE", old_auth_store);
        restore_env("KERUX_AUTH_REF", old_auth_ref);
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

        let agent = KeruxAgent::new(
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

    #[tokio::test]
    async fn build_messages_routes_search_replace_capability() {
        // Anthropic advertises EditFormat::SearchReplace; the hint must appear.
        let anthropic = AnthropicClient::new(crate::client::ClientConfig::default()).unwrap();
        let agent = KeruxAgent::new_with_provider(
            AgentConfig::default(),
            Arc::new(anthropic),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(system.contains("<edit_format>"));
        assert!(system.contains("edit_block"));

        // OpenAI advertises EditFormat::FullFile; no hint.
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(!system.contains("<edit_format>"));
    }

    #[tokio::test]
    async fn build_messages_edit_format_override_beats_capability_table() {
        // OpenAI's table says FullFile (no hint) for gpt-4; override forces SearchReplace.
        let config = AgentConfig {
            model: "gpt-4".to_string(),
            edit_format_override: Some(crate::client::EditFormat::SearchReplace),
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(system.contains("<edit_format>"));
        assert!(system.contains("edit_block"));

        // Override back to FullFile suppresses the hint even for Anthropic.
        let config = AgentConfig {
            edit_format_override: Some(crate::client::EditFormat::FullFile),
            ..AgentConfig::default()
        };
        let anthropic = AnthropicClient::new(crate::client::ClientConfig::default()).unwrap();
        let agent = KeruxAgent::new_with_provider(
            config,
            Arc::new(anthropic),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(!system.contains("<edit_format>"));
    }

    #[tokio::test]
    async fn build_messages_injects_repo_map_when_enabled() {
        // Enabled: current dir is the kerux repo itself, so tags exist.
        let config = AgentConfig {
            repo_map_tokens: 2048,
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(system.contains("<repo_map>"));
        assert!(system.contains("</repo_map>"));
    }

    #[tokio::test]
    async fn build_messages_skips_repo_map_when_disabled() {
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(!system.contains("<repo_map>"));
    }

    #[tokio::test]
    async fn build_messages_routes_patch_capability() {
        // o3 advertises EditFormat::Patch via the capability table.
        let config = AgentConfig {
            model: "o3-mini".to_string(),
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(system.contains("<edit_format>"));
        assert!(system.contains("`patch` tool"));
        assert!(!system.contains("edit_block"));

        // Known search/replace models still route to edit_block.
        let config = AgentConfig {
            model: "gpt-4o".to_string(),
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .first()
            .map(|m| m.content.as_str())
            .unwrap_or_default();
        assert!(system.contains("edit_block"));
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
        let second = router.feed("kerux!");
        let rest = router.finish();

        assert_eq!(first, "Halo ");
        assert_eq!(second, "kerux!");
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
        let agent = KeruxAgent::new(
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
        let agent = KeruxAgent::new(
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
        let agent = KeruxAgent::new(
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
        let agent = KeruxAgent::with_events(
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

        let no_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (content, _, _) = agent
            .process_stream(stream, &preflight, &no_cancel)
            .await
            .unwrap();

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
        let agent = KeruxAgent::with_events(
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

        let no_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (_, _, tool_calls) = agent
            .process_stream(stream, &preflight, &no_cancel)
            .await
            .unwrap();

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
        let agent = KeruxAgent::new(
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

        let no_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (content, reasoning, tool_calls) = agent
            .process_stream(stream, &preflight, &no_cancel)
            .await
            .unwrap();

        assert_eq!(content, "Hello from Claude");
        assert_eq!(reasoning, "checking");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "toolu_1");
        assert_eq!(tool_calls[0].function.name, "datetime");
        assert_eq!(tool_calls[0].function.arguments, "{}");
    }

    #[tokio::test]
    async fn process_stream_keeps_empty_anthropic_tool_input_valid_json() {
        let agent = KeruxAgent::new(
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

        let no_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (_, _, tool_calls) = agent
            .process_stream(stream, &preflight, &no_cancel)
            .await
            .unwrap();

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
    async fn warn_mode_continues_without_raw_fallback() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-recorder-warn-mode"),
        )
        .unwrap();
        journal
            .finalize(
                crate::run_journal::RunStatus::Succeeded,
                unix_timestamp_ms(),
            )
            .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::with_failure_mode(
            journal,
            crate::run_journal::RecorderFailureMode::Warn,
        ));
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(recorder));
        let secret = "Bearer sk-recorder-warn-fallback-secret";
        let events_path = runs_root
            .join("agent-recorder-warn-mode")
            .join("events.ndjson");
        let before = std::fs::read(&events_path).unwrap();

        let result = agent
            .emit(AgentEvent::ToolStart {
                call_id: "call-warn".to_string(),
                name: "example".to_string(),
                arguments: secret.to_string(),
            })
            .await;

        assert!(result.is_ok());
        let after = std::fs::read(&events_path).unwrap();
        assert_eq!(after, before);
        assert!(!String::from_utf8(after).unwrap().contains(secret));
    }

    #[tokio::test]
    async fn fail_mode_surfaces_recorder_errors_without_raw_fallback() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let mut journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-recorder-fail-mode"),
        )
        .unwrap();
        journal
            .finalize(
                crate::run_journal::RunStatus::Succeeded,
                unix_timestamp_ms(),
            )
            .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::with_failure_mode(
            journal,
            crate::run_journal::RecorderFailureMode::Fail,
        ));
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(recorder));
        let secret = "Bearer sk-recorder-fallback-secret";
        let events_path = runs_root
            .join("agent-recorder-fail-mode")
            .join("events.ndjson");
        let before = std::fs::read(&events_path).unwrap();

        let result = agent
            .emit(AgentEvent::ToolStart {
                call_id: "call-fail".to_string(),
                name: "example".to_string(),
                arguments: secret.to_string(),
            })
            .await;

        assert!(
            matches!(result, Err(Error::Agent(message)) if message.contains("run recorder failed"))
        );
        let after = std::fs::read(&events_path).unwrap();
        assert_eq!(after, before);
        assert!(!String::from_utf8(after).unwrap().contains(secret));
    }

    #[tokio::test]
    async fn attached_recorder_preserves_existing_event_delivery() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let manifest = test_run_manifest("agent-event-delivery");
        let journal = crate::run_journal::RunJournal::create_in(&runs_root, manifest).unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let agent = KeruxAgent::with_events(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
            tx,
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        agent
            .emit(AgentEvent::ToolStart {
                call_id: "call-42".to_string(),
                name: "example".to_string(),
                arguments: "{}".to_string(),
            })
            .await
            .unwrap();

        assert!(matches!(
            rx.recv().await,
            Some(AgentEvent::ToolStart { call_id, .. }) if call_id == "call-42"
        ));
        let events = recorder.events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "tool_started");
        let bounded: crate::redaction::BoundedPayload =
            serde_json::from_value(events[0].payload.clone()).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&bounded.content).unwrap();
        assert_eq!(payload["call_id"], "call-42");
    }

    #[tokio::test]
    async fn recorder_omits_raw_reasoning_and_keeps_only_metadata() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-reasoning-privacy"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));
        let raw_reasoning = "private chain of thought that must never be persisted";

        agent
            .emit(AgentEvent::Reasoning {
                text: raw_reasoning.to_string(),
            })
            .await
            .unwrap();

        let raw_journal = std::fs::read_to_string(
            runs_root
                .join("agent-reasoning-privacy")
                .join("events.ndjson"),
        )
        .unwrap();
        assert!(!raw_journal.contains(raw_reasoning));
        let events = recorder.events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "reasoning_metadata");
        let bounded: crate::redaction::BoundedPayload =
            serde_json::from_value(events[0].payload.clone()).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&bounded.content).unwrap();
        assert_eq!(payload["bytes"], raw_reasoning.len());
        assert_eq!(payload["sha256"].as_str().unwrap().len(), 64);
        assert!(payload.get("text").is_none());
    }

    #[tokio::test]
    async fn cancelled_run_records_exactly_one_terminal_status() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-cancelled-run"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let result = agent.run_with_cancel("stop now".to_string(), cancel).await;

        assert!(matches!(result, Err(Error::Cancelled)));
        let events = recorder.events().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "run_cancelled")
                .count(),
            1
        );
        let manifest: crate::run_journal::RunManifestV1 = serde_json::from_slice(
            &std::fs::read(runs_root.join("agent-cancelled-run").join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.status, crate::run_journal::RunStatus::Cancelled);
        assert!(recorder
            .finalize(
                crate::run_journal::RunStatus::Cancelled,
                unix_timestamp_ms()
            )
            .is_err());
    }

    #[tokio::test]
    async fn failed_run_records_exactly_one_terminal_status() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-failed-run"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let config = AgentConfig {
            max_iterations: 0,
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let result = agent
            .run_with_cancel(
                "fail before provider".to_string(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await;

        assert!(matches!(
            result,
            Err(Error::MaxIterationsExceeded { max: 0 })
        ));
        let events = recorder.events().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "run_failed")
                .count(),
            1
        );
        let manifest: crate::run_journal::RunManifestV1 = serde_json::from_slice(
            &std::fs::read(runs_root.join("agent-failed-run").join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.status, crate::run_journal::RunStatus::Failed);
        assert!(recorder
            .finalize(crate::run_journal::RunStatus::Failed, unix_timestamp_ms())
            .is_err());
    }

    #[tokio::test]
    async fn successful_run_records_exactly_one_terminal_status() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-successful-run"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let config = AgentConfig {
            stream: false,
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new_with_provider(
            config,
            Arc::new(StaticProvider),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let result = agent
            .run_with_cancel(
                "finish now".to_string(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap();

        assert_eq!(result.content, "done");
        let events = recorder.events().unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "run_started",
                "thinking_metadata",
                "telemetry",
                "request_prepared",
                "content_delta",
                "telemetry",
                "assistant_message",
                "run_completed",
            ]
        );
        let manifest: crate::run_journal::RunManifestV1 = serde_json::from_slice(
            &std::fs::read(runs_root.join("agent-successful-run").join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.status, crate::run_journal::RunStatus::Succeeded);
        assert!(recorder
            .finalize(
                crate::run_journal::RunStatus::Succeeded,
                unix_timestamp_ms()
            )
            .is_err());
    }

    #[tokio::test]
    async fn self_healing_finalizes_only_after_the_last_attempt() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-healed-run"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let provider = Arc::new(HealingProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let config = AgentConfig {
            stream: false,
            max_healing_attempts: 1,
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new_with_provider(
            config,
            provider.clone(),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let result = agent
            .run_with_healing("heal once".to_string())
            .await
            .unwrap();

        assert_eq!(result.content, "done");
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let events = recorder.events().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind.starts_with("run_"))
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["run_started", "run_completed"]
        );
        let manifest: crate::run_journal::RunManifestV1 = serde_json::from_slice(
            &std::fs::read(runs_root.join("agent-healed-run").join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.status, crate::run_journal::RunStatus::Succeeded);
    }

    #[tokio::test]
    async fn provider_failure_is_captured_by_the_terminal_boundary() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("agent-provider-failure"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let config = AgentConfig {
            stream: false,
            ..AgentConfig::default()
        };
        let agent = KeruxAgent::new_with_provider(
            config,
            Arc::new(FailingProvider),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let result = agent
            .run_with_cancel(
                "fail at provider".to_string(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await;

        assert!(matches!(result, Err(Error::Agent(message)) if message == "provider exploded"));
        let events = recorder.events().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "run_failed")
                .count(),
            1
        );
        let manifest: crate::run_journal::RunManifestV1 = serde_json::from_slice(
            &std::fs::read(
                runs_root
                    .join("agent-provider-failure")
                    .join("manifest.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.status, crate::run_journal::RunStatus::Failed);
    }

    #[tokio::test]
    async fn recorder_correlates_tool_start_and_completion_by_call_id() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let manifest = test_run_manifest("agent-tool-correlation");
        let journal = crate::run_journal::RunJournal::create_in(&runs_root, manifest).unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        agent
            .emit(AgentEvent::ToolStart {
                call_id: "call-shared".to_string(),
                name: "example".to_string(),
                arguments: "{}".to_string(),
            })
            .await
            .unwrap();
        agent
            .emit(AgentEvent::ToolComplete {
                result: ToolResult::success("call-shared", serde_json::json!({"ok": true})),
            })
            .await
            .unwrap();

        let events = recorder.events().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "tool_started");
        assert_eq!(events[1].kind, "tool_completed");
        for event in events {
            let bounded: crate::redaction::BoundedPayload =
                serde_json::from_value(event.payload).unwrap();
            let payload: serde_json::Value = serde_json::from_str(&bounded.content).unwrap();
            assert_eq!(payload["call_id"], "call-shared");
        }
    }

    #[tokio::test]
    async fn execute_tools_preserves_model_tool_call_id() {
        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry.register(WrongIdTool).await.unwrap();
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );

        let no_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = agent
            .execute_tools(
                vec![ToolCall {
                    id: "call_from_model".to_string(),
                    function: crate::client::ToolCallFunction {
                        name: "wrong_id".to_string(),
                        arguments: "{}".to_string(),
                    },
                }],
                &no_cancel,
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].tool_call_id, "call_from_model");
    }

    #[tokio::test]
    async fn execute_tools_returns_model_id_for_invalid_json() {
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        );

        let no_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = agent
            .execute_tools(
                vec![ToolCall {
                    id: "bad_json_call".to_string(),
                    function: crate::client::ToolCallFunction {
                        name: "wrong_id".to_string(),
                        arguments: "{".to_string(),
                    },
                }],
                &no_cancel,
            )
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

    /// Decode an event payload through the bounded redaction envelope.
    fn decode_event_payload(event: &crate::run_journal::RunEventEnvelope) -> serde_json::Value {
        let bounded: crate::redaction::BoundedPayload =
            serde_json::from_value(event.payload.clone()).unwrap();
        serde_json::from_str(&bounded.content).unwrap()
    }

    #[tokio::test]
    async fn edit_outcome_recorded_for_successful_search_replace() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("lib.rs");
        std::fs::write(&target, "fn main() {}\n").unwrap();

        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("edit-outcome-success"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));

        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry
            .register(crate::tools::EditBlockTool)
            .await
            .unwrap();
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let args = serde_json::json!({
            "path": target.to_string_lossy(),
            "edits": [{"search": "fn main() {}", "replace": "fn main() { println!(\"hi\"); }"}]
        });
        let results = agent
            .execute_tools(
                vec![ToolCall {
                    id: "call-edit-1".to_string(),
                    function: crate::client::ToolCallFunction {
                        name: "edit_block".to_string(),
                        arguments: args.to_string(),
                    },
                }],
                &no_cancel,
            )
            .await
            .unwrap();
        assert!(results[0].success, "edit should apply: {:?}", results[0]);

        let events = recorder.events().unwrap();
        let edit_events: Vec<_> = events.iter().filter(|e| e.kind == "edit_outcome").collect();
        assert_eq!(edit_events.len(), 1);
        let payload = decode_event_payload(edit_events[0]);
        assert_eq!(payload["call_id"], "call-edit-1");
        assert_eq!(payload["tool_name"], "edit_block");
        assert_eq!(payload["format"], "search_replace");
        assert_eq!(payload["parse_status"], "ok");
        assert_eq!(payload["apply_status"], "ok");
        assert_eq!(payload["match_type"], "exact");
        assert_eq!(payload["language"], "rust");
        assert_eq!(payload["repair_count"], 0);
        assert_eq!(payload["provider_kind"], "test-provider");
        assert_eq!(payload["model"], "test-model");
        assert_eq!(
            payload["path"],
            serde_json::Value::String(target.to_string_lossy().into_owned())
        );
    }

    #[tokio::test]
    async fn edit_outcome_tracks_parse_failures_and_repair_counts() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("app.py");
        std::fs::write(&target, "value = 1\n").unwrap();

        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("edit-outcome-repair"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));

        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry
            .register(crate::tools::EditBlockTool)
            .await
            .unwrap();
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let call = |id: &str, name: &str, arguments: String| ToolCall {
            id: id.to_string(),
            function: crate::client::ToolCallFunction {
                name: name.to_string(),
                arguments,
            },
        };
        let path_str = target.to_string_lossy().to_string();

        // 1. Invalid argument JSON → skipped, parse_status failed, and no
        //    target path could be extracted so the repair counter is untouched.
        // 2. Valid JSON but missing search text → failed, repair_count 1.
        // 3. Exact match → ok, repair_count 2 (successes never reset counters).
        let calls = vec![
            call("call-bad", "edit_block", "{\"path\":".to_string()),
            call(
                "call-miss",
                "edit_block",
                serde_json::json!({
                    "path": path_str,
                    "edits": [{"search": "text that is not in the file", "replace": "x"}]
                })
                .to_string(),
            ),
            call(
                "call-hit",
                "edit_block",
                serde_json::json!({
                    "path": path_str,
                    "edits": [{"search": "value = 1", "replace": "value = 2"}]
                })
                .to_string(),
            ),
        ];
        let results = agent.execute_tools(calls, &no_cancel).await.unwrap();
        assert!(!results[0].success);
        assert!(!results[1].success);
        assert!(results[2].success);

        let events = recorder.events().unwrap();
        let edit_events: Vec<_> = events.iter().filter(|e| e.kind == "edit_outcome").collect();
        assert_eq!(edit_events.len(), 3);

        let first = decode_event_payload(edit_events[0]);
        assert_eq!(first["apply_status"], "skipped");
        assert_eq!(first["parse_status"], "failed");
        assert_eq!(first["repair_count"], 0);

        let second = decode_event_payload(edit_events[1]);
        assert_eq!(second["apply_status"], "failed");
        assert_eq!(second["parse_status"], "ok");
        assert_eq!(second["repair_count"], 0);
        assert_eq!(second["language"], "python");

        let third = decode_event_payload(edit_events[2]);
        assert_eq!(third["apply_status"], "ok");
        assert_eq!(third["repair_count"], 1);
    }

    #[tokio::test]
    async fn edit_outcome_reports_pass_kind_and_repair_budget() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("app.py");
        std::fs::write(&target, "value = 1\n").unwrap();

        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("edit-outcome-pass-kind"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));

        let config = AgentConfig {
            max_repair_attempts: Some(1), // bounded repair policy under test
            ..Default::default()
        };
        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry
            .register(crate::tools::EditBlockTool)
            .await
            .unwrap();
        let agent = KeruxAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let call = |id: &str, arguments: String| ToolCall {
            id: id.to_string(),
            function: crate::client::ToolCallFunction {
                name: "edit_block".to_string(),
                arguments,
            },
        };
        let path_str = target.to_string_lossy().to_string();
        let miss = serde_json::json!({
            "path": path_str,
            "edits": [{"search": "text that is not in the file", "replace": "x"}]
        })
        .to_string();

        // Success, then two failures on the same path. `pass_kind` flips only
        // after a failure (evidence exists to repair from); with a budget of 1
        // the second failure exhausts the path, so it reports
        // `repair_allowed=false` while the counter keeps counting.
        agent
            .execute_tools(
                vec![
                    call(
                        "c0",
                        serde_json::json!({
                            "path": path_str,
                            "edits": [{"search": "value = 1", "replace": "value = 2"}]
                        })
                        .to_string(),
                    ),
                    call("c1", miss.clone()),
                    call("c2", miss),
                ],
                &no_cancel,
            )
            .await
            .unwrap();

        let events = recorder.events().unwrap();
        let edit_events: Vec<_> = events.iter().filter(|e| e.kind == "edit_outcome").collect();
        assert_eq!(edit_events.len(), 3);

        let first = decode_event_payload(edit_events[0]);
        assert_eq!(first["pass_kind"], "first_pass");
        assert_eq!(first["repair_allowed"], true);
        assert_eq!(first["run_attempt"], 1);

        let second = decode_event_payload(edit_events[1]);
        assert_eq!(second["pass_kind"], "first_pass");
        assert_eq!(second["repair_count"], 0);
        assert_eq!(second["repair_allowed"], true);

        let third = decode_event_payload(edit_events[2]);
        assert_eq!(third["pass_kind"], "repair_pass");
        assert_eq!(third["repair_count"], 1);
        assert_eq!(third["repair_allowed"], false);
    }

    #[tokio::test]
    async fn classified_edit_failure_promotes_fallback_hint() {
        use crate::edit_metrics::EditFormat as MetricsFormat;

        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("app.py");
        std::fs::write(&target, "value = 1\n").unwrap();

        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("edit-fallback-promote"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));

        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry
            .register(crate::tools::EditBlockTool)
            .await
            .unwrap();
        let agent = KeruxAgent::new(
            AgentConfig::default(), // unknown model → FullFile base routing
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let miss = serde_json::json!({
            "path": target.to_string_lossy(),
            "edits": [{"search": "text that is not in the file", "replace": "x"}]
        })
        .to_string();

        // A classified edit-application failure must promote the run's
        // one-way fallback hint (search_replace → patch) ...
        let results = agent
            .execute_tools(
                vec![ToolCall {
                    id: "call-fb-1".to_string(),
                    function: crate::client::ToolCallFunction {
                        name: "edit_block".to_string(),
                        arguments: miss,
                    },
                }],
                &no_cancel,
            )
            .await
            .unwrap();
        assert!(!results[0].success);
        assert_eq!(
            agent.edit_metrics.lock().unwrap().format_hint(),
            Some(MetricsFormat::Patch)
        );

        // ... and build_messages must now route the stronger protocol.
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .iter()
            .find(|m| matches!(m.role, crate::client::Role::System))
            .unwrap();
        assert!(
            system.content.contains("prefers targeted patches"),
            "expected patch routing after fallback promotion, got: {}",
            system.content
        );

        let events = recorder.events().unwrap();
        let edit_events: Vec<_> = events.iter().filter(|e| e.kind == "edit_outcome").collect();
        assert_eq!(edit_events.len(), 1);
        let payload = decode_event_payload(edit_events[0]);
        assert_eq!(payload["effective_format"], "patch");
    }

    #[tokio::test]
    async fn fallback_hint_never_demotes_and_override_wins() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("app.py");
        std::fs::write(&target, "value = 1\n").unwrap();

        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry
            .register(crate::tools::EditBlockTool)
            .await
            .unwrap();
        let config = AgentConfig {
            edit_format_override: Some(crate::client::EditFormat::SearchReplace),
            ..Default::default()
        };
        let agent = KeruxAgent::new(
            config,
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );

        // Drive the tracker straight to the terminal rung.
        {
            let mut t = agent.edit_metrics.lock().unwrap();
            t.record_fallback(
                target.to_string_lossy().as_ref(),
                crate::edit_metrics::EditFormat::Patch,
            );
            assert_eq!(
                t.format_hint(),
                Some(crate::edit_metrics::EditFormat::FullFile)
            );
        }

        // Explicit config override always wins over the learned hint.
        let messages = agent.build_messages().await.unwrap();
        let system = messages
            .iter()
            .find(|m| matches!(m.role, crate::client::Role::System))
            .unwrap();
        assert!(system
            .content
            .contains("token-efficient search/replace edits"));

        // Weaker-protocol failures never demote the existing stronger hint.
        agent
            .edit_metrics
            .lock()
            .unwrap()
            .record_fallback("other.py", crate::edit_metrics::EditFormat::SearchReplace);
        assert_eq!(
            agent.edit_metrics.lock().unwrap().format_hint(),
            Some(crate::edit_metrics::EditFormat::FullFile)
        );
    }

    #[tokio::test]
    async fn non_edit_tools_produce_no_edit_outcome() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("edit-outcome-non-edit"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));

        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry.register(WrongIdTool).await.unwrap();
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = agent
            .execute_tools(
                vec![ToolCall {
                    id: "call-non-edit".to_string(),
                    function: crate::client::ToolCallFunction {
                        name: "wrong_id".to_string(),
                        arguments: "{}".to_string(),
                    },
                }],
                &no_cancel,
            )
            .await
            .unwrap();
        assert!(results[0].success);

        let events = recorder.events().unwrap();
        assert!(
            events.iter().all(|e| e.kind != "edit_outcome"),
            "non-edit tools must not emit edit_outcome events"
        );
    }

    #[tokio::test]
    async fn request_prepared_records_full_provenance() {
        let home = tempfile::tempdir().unwrap();
        let runs_root = home.path().join("runs");
        let journal = crate::run_journal::RunJournal::create_in(
            &runs_root,
            test_run_manifest("request-prepared-provenance"),
        )
        .unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));

        let memory = crate::memory::MemoryManager::new();
        memory
            .store(
                crate::memory::MemoryBlock::new("mem-1", "fact", "important fact").importance(80),
            )
            .await;

        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            ToolRegistry::new(Duration::from_secs(1)),
        )
        .with_memory_manager(memory);
        agent.set_run_recorder(Some(Arc::clone(&recorder)));

        let messages = vec![Message::system("You are helpful."), Message::user("Hello")];
        let tools = vec![crate::schema::ToolSchema {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let telemetry = AgentTelemetry {
            prompt_tokens: 42,
            completion_tokens: 0,
            total_tokens: 42,
            context_window: 128_000,
            compacted: false,
            estimated: true,
            billable: false,
        };

        agent
            .record_request_prepared(0, &messages, &tools, &telemetry)
            .await
            .unwrap();

        let events = recorder.events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "request_prepared");

        let bounded: crate::redaction::BoundedPayload =
            serde_json::from_value(events[0].payload.clone()).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&bounded.content).unwrap();

        // Core fields
        assert_eq!(payload["iteration"], 0);
        assert_eq!(payload["message_count"], 2);
        assert_eq!(payload["model"], "gpt-4");

        // Message digests: one per message, each with role + sha256 + tokens
        let digests = payload["message_digests"].as_array().unwrap();
        assert_eq!(digests.len(), 2);
        assert_eq!(digests[0]["role"], "System");
        assert_eq!(digests[1]["role"], "User");
        assert!(digests[0]["sha256"].as_str().unwrap().len() == 64);
        assert!(digests[0]["tokens"].as_u64().unwrap() > 0);

        // Tool schemas recorded with name + digest
        let tool_schemas = payload["tool_schemas"].as_array().unwrap();
        assert_eq!(tool_schemas.len(), 1);
        assert_eq!(tool_schemas[0]["name"], "test_tool");
        assert!(tool_schemas[0]["sha256"].as_str().unwrap().len() == 64);

        // Memory blocks: importance >= 70 captured
        let memory_blocks = payload["memory_blocks"].as_array().unwrap();
        assert_eq!(memory_blocks.len(), 1);
        assert_eq!(memory_blocks[0]["id"], "mem-1");
        assert_eq!(memory_blocks[0]["importance"], 80);

        // Capabilities present
        assert!(payload["capabilities"]["supports_tool_calls"].is_boolean());

        // Telemetry snapshot
        assert_eq!(payload["telemetry"]["prompt_tokens"], 42);
        assert_eq!(payload["telemetry"]["compacted"], false);

        // Skills array present (empty until wired)
        assert!(payload["skills"].is_array());
    }

    // ── Task 1.5: approval decision recording ──────────────────────────

    /// A tool registered under a dangerous name so the approval gate fires.
    struct DangerousTool;

    #[async_trait]
    impl crate::tools::KeruxTool for DangerousTool {
        fn name(&self) -> &str {
            "terminal"
        }

        fn description(&self) -> &str {
            "A dangerous tool for approval tests"
        }

        fn schema(&self) -> crate::schema::ToolSchema {
            crate::schema::ToolSchema::new(
                "terminal",
                "A dangerous tool for approval tests",
                serde_json::json!({ "type": "object", "properties": {} }),
            )
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _context: crate::tools::ToolContext,
        ) -> ToolResult {
            ToolResult::success("dangerous_call", serde_json::json!({ "ran": true }))
        }
    }

    /// A gate that returns a fixed decision.
    struct FixedGate(crate::approval::ApprovalDecision);

    #[async_trait]
    impl crate::approval::ToolApprovalGate for FixedGate {
        async fn request_approval(
            &self,
            _request: crate::approval::ApprovalRequest,
        ) -> crate::approval::ApprovalDecision {
            self.0.clone()
        }
    }

    /// Build an agent with a recorder, a dangerous tool, and a fixed gate.
    async fn approval_test_agent(
        recorder: Arc<crate::run_journal::RunRecorder>,
        decision: crate::approval::ApprovalDecision,
    ) -> KeruxAgent {
        let registry = ToolRegistry::new(Duration::from_secs(1));
        registry.register(DangerousTool).await.unwrap();
        let agent = KeruxAgent::new(
            AgentConfig::default(),
            OpenAIClient::new(crate::client::ClientConfig::default()),
            registry,
        );
        agent.set_run_recorder(Some(Arc::clone(&recorder)));
        agent.set_approval_gate(Some(Arc::new(FixedGate(decision))));
        agent
    }

    fn dangerous_tool_call() -> ToolCall {
        ToolCall {
            id: "call_dangerous".to_string(),
            function: crate::client::ToolCallFunction {
                name: "terminal".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    fn approval_events(
        recorder: &crate::run_journal::RunRecorder,
    ) -> Vec<(String, serde_json::Value)> {
        recorder
            .events()
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "approval_decision")
            .map(|e| {
                let bounded: crate::redaction::BoundedPayload =
                    serde_json::from_value(e.payload).unwrap();
                let payload: serde_json::Value = serde_json::from_str(&bounded.content).unwrap();
                (e.kind, payload)
            })
            .collect()
    }

    #[tokio::test]
    async fn approval_approved_records_decision_and_runs_tool() {
        let home = tempfile::tempdir().unwrap();
        let manifest = test_run_manifest("approval-approved");
        let journal =
            crate::run_journal::RunJournal::create_in(home.path().join("runs"), manifest).unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = approval_test_agent(
            Arc::clone(&recorder),
            crate::approval::ApprovalDecision::Approved,
        )
        .await;

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = agent
            .execute_tools(vec![dangerous_tool_call()], &no_cancel)
            .await
            .unwrap();

        // Tool ran.
        assert_eq!(results.len(), 1);
        assert!(results[0].success);

        // Decision journaled and correlated by call_id.
        let events = approval_events(&recorder);
        assert_eq!(events.len(), 1);
        let payload = &events[0].1;
        assert_eq!(payload["call_id"], "call_dangerous");
        assert_eq!(payload["tool_name"], "terminal");
        assert_eq!(payload["approved"], true);
        assert_eq!(payload["outcome"], "approved");
        assert!(payload["reason"].is_null());
    }

    #[tokio::test]
    async fn approval_denied_records_redacted_reason_and_blocks_tool() {
        let home = tempfile::tempdir().unwrap();
        let manifest = test_run_manifest("approval-denied");
        let journal =
            crate::run_journal::RunJournal::create_in(home.path().join("runs"), manifest).unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = approval_test_agent(
            Arc::clone(&recorder),
            crate::approval::ApprovalDecision::Denied {
                reason: "not allowed: sk-secret12345678".to_string(),
                outcome: crate::approval::ApprovalOutcome::Denied,
            },
        )
        .await;

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = agent
            .execute_tools(vec![dangerous_tool_call()], &no_cancel)
            .await
            .unwrap();

        // Tool blocked; denial reason fed back as the tool error.
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert!(results[0].error.as_deref().unwrap().contains("not allowed"));

        // Decision journaled with the secret redacted.
        let events = approval_events(&recorder);
        assert_eq!(events.len(), 1);
        let payload = &events[0].1;
        assert_eq!(payload["call_id"], "call_dangerous");
        assert_eq!(payload["approved"], false);
        assert_eq!(payload["outcome"], "denied");
        let reason = payload["reason"].as_str().unwrap();
        assert!(reason.contains("not allowed"));
        assert!(!reason.contains("sk-secret12345678"));
    }

    #[tokio::test]
    async fn approval_timeout_records_auto_deny() {
        let home = tempfile::tempdir().unwrap();
        let manifest = test_run_manifest("approval-timeout");
        let journal =
            crate::run_journal::RunJournal::create_in(home.path().join("runs"), manifest).unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = approval_test_agent(
            Arc::clone(&recorder),
            crate::approval::ApprovalDecision::Denied {
                reason: "Approval timed out; tool execution denied.".to_string(),
                outcome: crate::approval::ApprovalOutcome::Timeout,
            },
        )
        .await;

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = agent
            .execute_tools(vec![dangerous_tool_call()], &no_cancel)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(!results[0].success);

        let events = approval_events(&recorder);
        assert_eq!(events.len(), 1);
        let payload = &events[0].1;
        assert_eq!(payload["approved"], false);
        assert_eq!(payload["outcome"], "timeout");
    }

    #[tokio::test]
    async fn approval_channel_closed_records_stale_response() {
        let home = tempfile::tempdir().unwrap();
        let manifest = test_run_manifest("approval-stale");
        let journal =
            crate::run_journal::RunJournal::create_in(home.path().join("runs"), manifest).unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = approval_test_agent(
            Arc::clone(&recorder),
            crate::approval::ApprovalDecision::Denied {
                reason: "Approval channel closed unexpectedly.".to_string(),
                outcome: crate::approval::ApprovalOutcome::ChannelClosed,
            },
        )
        .await;

        let no_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let results = agent
            .execute_tools(vec![dangerous_tool_call()], &no_cancel)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(!results[0].success);

        let events = approval_events(&recorder);
        assert_eq!(events.len(), 1);
        let payload = &events[0].1;
        assert_eq!(payload["approved"], false);
        assert_eq!(payload["outcome"], "channel_closed");
    }

    #[tokio::test]
    async fn approval_cancelled_before_gate_records_nothing_and_blocks_tool() {
        let home = tempfile::tempdir().unwrap();
        let manifest = test_run_manifest("approval-cancelled");
        let journal =
            crate::run_journal::RunJournal::create_in(home.path().join("runs"), manifest).unwrap();
        let recorder = Arc::new(crate::run_journal::RunRecorder::new(journal));
        let agent = approval_test_agent(
            Arc::clone(&recorder),
            crate::approval::ApprovalDecision::Approved,
        )
        .await;

        // Cancel flag set before the tool loop reaches the gate.
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let results = agent
            .execute_tools(vec![dangerous_tool_call()], &cancelled)
            .await
            .unwrap();

        // Tool never reached the gate: placeholder error, no approval event.
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert!(results[0].error.as_deref().unwrap().contains("cancelled"));
        assert!(approval_events(&recorder).is_empty());
    }
}
