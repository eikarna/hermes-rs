use std::path::PathBuf;

use kerux_core::agent::{AgentEvent, AgentTelemetry};
use kerux_core::client::Message;
use kerux_core::config::{AppConfig, BehaviorSettings, McpTransportKind};

use crate::tui::forms::Modal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Landing,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Wide,
    Medium,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePanel {
    Session,
    Mcp,
    Skills,
    Behavior,
}

impl ActivePanel {
    pub fn all() -> [Self; 4] {
        [Self::Session, Self::Mcp, Self::Skills, Self::Behavior]
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Mcp => "MCP",
            Self::Skills => "Skills",
            Self::Behavior => "Behavior",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Session => Self::Mcp,
            Self::Mcp => Self::Skills,
            Self::Skills => Self::Behavior,
            Self::Behavior => Self::Session,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Session => Self::Behavior,
            Self::Mcp => Self::Session,
            Self::Skills => Self::Mcp,
            Self::Behavior => Self::Skills,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Prompt,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct ActivityItem {
    pub label: String,
    pub body: String,
    pub tone: Tone,
}

#[derive(Debug, Clone)]
pub struct FooterNotice {
    pub text: String,
    pub tone: Tone,
}

#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub role: &'static str,
    pub content: String,
    pub timestamp: Option<String>,
    pub tools: Vec<ToolCallLine>,
}

impl TranscriptEntry {
    pub fn new(role: &'static str, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            timestamp: Some(now_hhmm()),
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolCallLine {
    pub name: String,
    pub ok: bool,
    pub duration_secs: Option<f64>,
}

fn now_hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

#[derive(Debug, Clone, Default)]
pub struct TelemetryState {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub context_window: usize,
    pub compacted: bool,
    pub estimated: bool,
    pub total_cost: f64,
    pub tokens_per_second: Option<f64>,
    pub turns_completed: usize,
    pub context_window_usage_pct: Option<f64>,
    pub cached_prompt_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub title: String,
    pub transcript: Vec<TranscriptEntry>,
    pub active_query: String,
    pub streaming_response: String,
    pub reasoning: String,
    pub activity: Vec<ActivityItem>,
    pub status: String,
    pub current_iteration: usize,
    pub max_iterations: usize,
    pub error: Option<String>,
    pub final_message: Option<String>,
    pub running: bool,
    pub telemetry: TelemetryState,
    pub pending_tools: Vec<ToolCallLine>,
    tool_starts: Vec<(String, String, std::time::Instant)>,
}

impl SessionState {
    pub fn new(max_iterations: usize) -> Self {
        Self {
            title: "New session".to_string(),
            transcript: Vec::new(),
            active_query: String::new(),
            streaming_response: String::new(),
            reasoning: String::new(),
            activity: vec![ActivityItem {
                label: "Ready".to_string(),
                body: "Waiting for your first prompt.".to_string(),
                tone: Tone::Info,
            }],
            status: "Idle".to_string(),
            current_iteration: 0,
            max_iterations,
            error: None,
            final_message: None,
            running: false,
            telemetry: TelemetryState::default(),
            pending_tools: Vec::new(),
            tool_starts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpServerItem {
    pub name: String,
    pub transport: McpTransportKind,
    pub endpoint: String,
    pub enabled: bool,
    pub connected: bool,
    pub tool_count: usize,
}

#[derive(Debug, Clone)]
pub struct SkillItem {
    pub name: String,
    pub description: String,
    pub version: String,
    pub available: bool,
    /// Awaiting approval in `_pending/`; never auto-loaded.
    pub pending: bool,
}

#[derive(Debug, Clone)]
pub struct PersistentState {
    pub config: AppConfig,
    pub behavior: BehaviorSettings,
    pub skills_root: PathBuf,
    pub mcp_servers: Vec<McpServerItem>,
    pub skills: Vec<SkillItem>,
    pub needs_rebuild: bool,
}

#[derive(Debug, Clone)]
pub struct UiState {
    pub view: ViewMode,
    pub layout: LayoutMode,
    pub active_panel: ActivePanel,
    pub input_mode: InputMode,
    pub conversation_scroll: u16,
    pub conversation_follow_tail: bool,
    pub prompt_input: String,
    pub pending_shell_command: Option<String>,
    pub prompt_history: Vec<String>,
    pub prompt_history_index: Option<usize>,
    pub prompt_history_draft: Option<String>,
    pub selected_mcp: usize,
    pub selected_skill: usize,
    pub selected_behavior: usize,
    pub footer_help: String,
    pub footer_notice: Option<FooterNotice>,
    pub modal: Option<Modal>,
    pub should_quit: bool,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub persistent: PersistentState,
    pub session: SessionState,
    pub ui: UiState,
}

impl AppState {
    pub fn new(config: AppConfig, prompt: String, start_in_workspace: bool) -> Self {
        let max_iterations = config.agent.max_iterations;
        Self {
            persistent: PersistentState {
                behavior: config.agent.clone(),
                skills_root: config.skills.root_dir.clone(),
                config,
                mcp_servers: Vec::new(),
                skills: Vec::new(),
                needs_rebuild: false,
            },
            session: SessionState::new(max_iterations),
            ui: UiState {
                view: if start_in_workspace {
                    ViewMode::Workspace
                } else {
                    ViewMode::Landing
                },
                layout: LayoutMode::Wide,
                active_panel: ActivePanel::Session,
                input_mode: if start_in_workspace {
                    InputMode::Prompt
                } else {
                    InputMode::Command
                },
                conversation_scroll: 0,
                conversation_follow_tail: true,
                prompt_input: prompt,
                pending_shell_command: None,
                prompt_history: Vec::new(),
                prompt_history_index: None,
                prompt_history_draft: None,
                selected_mcp: 0,
                selected_skill: 0,
                selected_behavior: 0,
                footer_help: "tab panels  ! shell  ctrl+l new session  q quit".to_string(),
                footer_notice: None,
                modal: None,
                should_quit: false,
            },
        }
    }

    pub fn set_layout_for_width(&mut self, width: u16) {
        self.ui.layout = if width < self.persistent.config.tui.compact_width {
            LayoutMode::Compact
        } else if width < self.persistent.config.tui.medium_width {
            LayoutMode::Medium
        } else {
            LayoutMode::Wide
        };
    }

    pub fn behavior_rows(&self) -> Vec<(String, String)> {
        let behavior = &self.persistent.behavior;
        vec![
            ("model".to_string(), behavior.model.clone()),
            (
                "system_prompt".to_string(),
                behavior
                    .system_prompt
                    .clone()
                    .unwrap_or_else(|| "(default)".to_string()),
            ),
            (
                "max_iterations".to_string(),
                behavior.max_iterations.to_string(),
            ),
            (
                "tool_timeout_secs".to_string(),
                behavior.tool_timeout_secs.to_string(),
            ),
            (
                "request_timeout_secs".to_string(),
                behavior.request_timeout_secs.to_string(),
            ),
            (
                "context_window".to_string(),
                behavior.context_window.to_string(),
            ),
            ("stream".to_string(), behavior.stream.to_string()),
            (
                "show_reasoning".to_string(),
                behavior.show_reasoning.to_string(),
            ),
            (
                "max_healing_attempts".to_string(),
                behavior.max_healing_attempts.to_string(),
            ),
        ]
    }

    pub fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Thinking { content } => {
                self.session.status = content.clone();
                self.push_activity("Thinking", &content, Tone::Info);
            }
            AgentEvent::Reasoning { text } => {
                self.session.reasoning.push_str(&text);
                self.session.status = "Streaming reasoning".to_string();
            }
            AgentEvent::ToolStart {
                call_id,
                name,
                arguments,
            } => {
                self.session
                    .tool_starts
                    .push((call_id, name.clone(), std::time::Instant::now()));
                self.push_activity(
                    format!("Tool {}", name),
                    truncate(&arguments, 140),
                    Tone::Warning,
                );
                self.session.status = format!("Running {}", name);
            }
            AgentEvent::ToolComplete { result } => {
                let started = self
                    .session
                    .tool_starts
                    .iter()
                    .position(|(id, _, _)| *id == result.tool_call_id)
                    .map(|pos| self.session.tool_starts.remove(pos));
                let (name, duration_secs) = match started {
                    Some((_, name, at)) => (name, Some(at.elapsed().as_secs_f64())),
                    None => (
                        result.tool_call_id.chars().take(12).collect::<String>(),
                        None,
                    ),
                };
                self.session.pending_tools.push(ToolCallLine {
                    name,
                    ok: result.success,
                    duration_secs,
                });
                self.push_activity(
                    "Tool complete",
                    truncate(&result.content, 160),
                    if result.success {
                        Tone::Success
                    } else {
                        Tone::Error
                    },
                );
                self.session.status = "Tool completed".to_string();
            }
            AgentEvent::ToolError { name, error } => {
                self.session.pending_tools.push(ToolCallLine {
                    name: name.clone(),
                    ok: false,
                    duration_secs: None,
                });
                self.push_activity(format!("Tool {}", name), error.clone(), Tone::Error);
                self.session.status = format!("{} failed", name);
            }
            AgentEvent::Content { text } => {
                self.session.streaming_response.push_str(&text);
                self.session.status = "Streaming response".to_string();
            }
            AgentEvent::Done { message } => self.finish_run(message),
            AgentEvent::IterationComplete { iteration } => {
                self.session.current_iteration = iteration;
                self.push_activity(
                    format!("Iteration {}", iteration),
                    "Agent loop step finished.",
                    Tone::Info,
                );
            }
            AgentEvent::Telemetry { telemetry } => self.apply_telemetry(telemetry),
            AgentEvent::BudgetAlert {
                action,
                reason,
                current_run_cost,
                daily_cost,
                downgrade_model,
            } => {
                let label = match action {
                    Some(action) => format!("Budget {:?} alert", action),
                    None => "Budget warning".to_string(),
                };
                let mut detail = format!(
                    "{} (run ${:.4}, day ${:.4}",
                    reason, current_run_cost, daily_cost
                );
                if let Some(model) = downgrade_model {
                    detail.push_str(&format!(", downgrade → {}", model));
                }
                detail.push(')');
                self.push_activity(label, detail, Tone::Warning);
                self.session.status = "Budget alert".to_string();
            }
            AgentEvent::Error { error } => {
                self.session.error = Some(error.clone());
                self.session.status = "Errored".to_string();
                self.push_activity("Error", error, Tone::Error);
            }
        }
    }

    pub fn begin_run(&mut self, query: String) {
        self.ui.view = ViewMode::Workspace;
        self.ui.active_panel = ActivePanel::Session;
        self.ui.input_mode = InputMode::Command;
        self.ui.conversation_scroll = 0;
        self.ui.conversation_follow_tail = true;
        self.remember_prompt(&query);
        self.clear_footer_notice();
        self.session.running = true;
        if self.session.transcript.is_empty() {
            self.session.title = derive_session_title(&query);
        }
        self.session.error = None;
        self.session.final_message = None;
        self.session.active_query = query.clone();
        self.session.streaming_response.clear();
        self.session.reasoning.clear();
        self.session.pending_tools.clear();
        self.session.tool_starts.clear();
        self.session.current_iteration = 0;
        self.session.status = "Requesting model response".to_string();
        self.session
            .transcript
            .push(TranscriptEntry::new("User", query));
    }

    pub fn begin_shell_run(&mut self, command: String) {
        self.ui.view = ViewMode::Workspace;
        self.ui.active_panel = ActivePanel::Session;
        self.ui.input_mode = InputMode::Command;
        self.ui.conversation_scroll = 0;
        self.ui.conversation_follow_tail = true;
        self.remember_prompt(&format!("!{}", command));
        self.clear_footer_notice();
        self.session.running = true;
        if self.session.transcript.is_empty() {
            self.session.title = derive_session_title(&format!("shell: {}", command));
        }
        self.session.error = None;
        self.session.final_message = None;
        self.session.active_query = format!("$ {}", command);
        self.session.streaming_response.clear();
        self.session.reasoning.clear();
        self.session.pending_tools.clear();
        self.session.tool_starts.clear();
        self.session.current_iteration = 0;
        self.session.status = "Running shell command".to_string();
        self.session
            .transcript
            .push(TranscriptEntry::new("Shell", command));
    }

    pub fn fail_run(&mut self, error: String) {
        self.session.running = false;
        self.session.pending_tools.clear();
        self.session.tool_starts.clear();
        self.session.error = Some(error.clone());
        self.session.status = "Run failed".to_string();
        self.ui.input_mode = InputMode::Prompt;
        self.record_app_event(
            "Run failed",
            error,
            Tone::Error,
            Some("follow-up prompt ready".to_string()),
        );
    }

    pub fn clear_session(&mut self) {
        let max_iterations = self.persistent.behavior.max_iterations;
        self.session = SessionState::new(max_iterations);
        self.ui.prompt_input.clear();
        self.ui.pending_shell_command = None;
        self.ui.view = ViewMode::Landing;
        self.ui.input_mode = InputMode::Command;
        self.ui.conversation_scroll = 0;
        self.ui.conversation_follow_tail = true;
        self.ui.prompt_history_index = None;
        self.ui.prompt_history_draft = None;
        self.clear_footer_notice();
    }

    pub fn scroll_conversation_up(&mut self, amount: u16) {
        self.ui.conversation_follow_tail = false;
        self.ui.conversation_scroll = self.ui.conversation_scroll.saturating_sub(amount);
    }

    pub fn scroll_conversation_down(&mut self, amount: u16) {
        self.ui.conversation_follow_tail = false;
        self.ui.conversation_scroll = self.ui.conversation_scroll.saturating_add(amount);
    }

    pub fn scroll_conversation_to_top(&mut self) {
        self.ui.conversation_follow_tail = false;
        self.ui.conversation_scroll = 0;
    }

    pub fn conversation_scroll(&self) -> u16 {
        self.ui.conversation_scroll
    }

    pub fn follow_conversation_tail(&self) -> bool {
        self.ui.conversation_follow_tail
    }

    pub fn prompt_history_previous(&mut self) {
        self.ui.pending_shell_command = None;
        if self.ui.prompt_history.is_empty() {
            return;
        }

        match self.ui.prompt_history_index {
            Some(index) if index > 0 => {
                self.ui.prompt_history_index = Some(index - 1);
                self.ui.prompt_input = self.ui.prompt_history[index - 1].clone();
            }
            Some(_) => {}
            None => {
                self.ui.prompt_history_draft = Some(self.ui.prompt_input.clone());
                let index = self.ui.prompt_history.len() - 1;
                self.ui.prompt_history_index = Some(index);
                self.ui.prompt_input = self.ui.prompt_history[index].clone();
            }
        }
    }

    pub fn prompt_history_next(&mut self) {
        self.ui.pending_shell_command = None;
        let Some(index) = self.ui.prompt_history_index else {
            return;
        };

        if index + 1 < self.ui.prompt_history.len() {
            self.ui.prompt_history_index = Some(index + 1);
            self.ui.prompt_input = self.ui.prompt_history[index + 1].clone();
        } else {
            self.ui.prompt_history_index = None;
            self.ui.prompt_input = self.ui.prompt_history_draft.take().unwrap_or_default();
        }
    }

    pub fn detach_prompt_history_navigation(&mut self) {
        self.ui.prompt_history_index = None;
        self.ui.prompt_history_draft = None;
    }

    pub fn push_activity(&mut self, label: impl Into<String>, body: impl Into<String>, tone: Tone) {
        self.session.activity.push(ActivityItem {
            label: label.into(),
            body: body.into(),
            tone,
        });
    }

    pub fn record_app_event(
        &mut self,
        label: impl Into<String>,
        body: impl Into<String>,
        tone: Tone,
        notice: Option<String>,
    ) {
        self.push_activity(label, body, tone);
        if let Some(text) = notice {
            self.set_footer_notice(text, tone);
        }
    }

    pub fn set_footer_notice(&mut self, text: impl Into<String>, tone: Tone) {
        self.ui.footer_notice = Some(FooterNotice {
            text: text.into(),
            tone,
        });
    }

    pub fn clear_footer_notice(&mut self) {
        self.ui.footer_notice = None;
    }

    fn remember_prompt(&mut self, prompt: &str) {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return;
        }
        if self
            .ui
            .prompt_history
            .last()
            .is_some_and(|last| last == prompt)
        {
            self.detach_prompt_history_navigation();
            return;
        }
        self.ui.prompt_history.push(prompt.to_string());
        self.detach_prompt_history_navigation();
    }

    fn finish_run(&mut self, message: Message) {
        let content = choose_final_content(&self.session.streaming_response, &message.content);
        let reasoning = choose_final_reasoning(
            &self.session.reasoning,
            message.reasoning.as_deref().unwrap_or(""),
        );
        self.session.streaming_response = content.clone();
        self.session.running = false;
        self.session.final_message = Some(content.clone());
        self.session.status = "Completed".to_string();
        self.ui.input_mode = InputMode::Prompt;
        self.ui.conversation_scroll = 0;
        self.ui.conversation_follow_tail = true;
        let mut entry = TranscriptEntry::new("Assistant", content);
        entry.tools = std::mem::take(&mut self.session.pending_tools);
        self.session.transcript.push(entry);
        if !reasoning.is_empty() {
            self.session.reasoning = reasoning;
        }
        self.push_activity("Done", "Response finished.", Tone::Success);
        self.set_footer_notice("follow-up prompt ready", Tone::Success);
    }

    fn apply_telemetry(&mut self, telemetry: AgentTelemetry) {
        if !self.persistent.config.telemetry.enabled {
            return;
        }

        if telemetry.billable {
            if let Some(cost) = telemetry.estimated_cost_usd {
                self.session.telemetry.total_cost += cost;
            } else {
                self.session.telemetry.total_cost += self.telemetry_cost(&telemetry);
            }
        }

        self.session.telemetry.prompt_tokens = telemetry.prompt_tokens;
        self.session.telemetry.completion_tokens = telemetry.completion_tokens;
        self.session.telemetry.total_tokens = telemetry.total_tokens;
        self.session.telemetry.context_window = telemetry.context_window;
        self.session.telemetry.compacted = telemetry.compacted;
        self.session.telemetry.estimated = telemetry.estimated;
        self.session.telemetry.tokens_per_second = telemetry.tokens_per_second;
        self.session.telemetry.turns_completed = telemetry.turns_completed;
        self.session.telemetry.context_window_usage_pct = telemetry.context_window_usage_pct;
        self.session.telemetry.cached_prompt_tokens = telemetry.cached_prompt_tokens;
    }

    fn telemetry_cost(&self, telemetry: &AgentTelemetry) -> f64 {
        let settings = &self.persistent.config.telemetry;
        (telemetry.prompt_tokens as f64 / 1_000_000.0) * settings.input_cost_per_million
            + (telemetry.completion_tokens as f64 / 1_000_000.0) * settings.output_cost_per_million
    }
}

fn derive_session_title(prompt: &str) -> String {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "New session".to_string();
    }

    let chars = normalized.chars().collect::<Vec<_>>();
    let mut title = String::new();
    let mut sentence_count = 0;
    for (index, ch) in chars.iter().copied().enumerate() {
        title.push(ch);
        if is_sentence_boundary(&chars, index) {
            sentence_count += 1;
            if sentence_count >= 2 {
                break;
            }
        }
    }

    if sentence_count == 0 {
        title = normalized;
    }

    truncate(&title, 64)
}

fn is_sentence_boundary(chars: &[char], index: usize) -> bool {
    if !matches!(chars[index], '.' | '!' | '?') {
        return false;
    }
    if is_known_abbreviation_at(chars, index) {
        return false;
    }

    chars[index + 1..]
        .iter()
        .copied()
        .find(|ch| !ch.is_whitespace())
        .is_none_or(char::is_uppercase)
}

fn is_known_abbreviation_at(chars: &[char], index: usize) -> bool {
    let prefix = chars[..=index].iter().collect::<String>();
    let token = prefix.split_whitespace().last().unwrap_or_default();
    matches!(
        token.to_ascii_lowercase().as_str(),
        "e.g."
            | "i.e."
            | "u.s."
            | "u.k."
            | "etc."
            | "mr."
            | "mrs."
            | "ms."
            | "dr."
            | "prof."
            | "sr."
            | "jr."
            | "vs."
    )
}

fn choose_final_content(streamed: &str, final_message: &str) -> String {
    let streamed = streamed.trim();
    let final_message = final_message.trim();

    if streamed.is_empty() {
        return final_message.to_string();
    }
    if final_message.is_empty() {
        return streamed.to_string();
    }

    if final_message.chars().count() > streamed.chars().count()
        && final_message.starts_with(streamed)
    {
        return final_message.to_string();
    }

    streamed.to_string()
}

fn choose_final_reasoning(streamed: &str, final_reasoning: &str) -> String {
    let streamed = streamed.trim();
    let final_reasoning = final_reasoning.trim();

    if streamed.is_empty() {
        return final_reasoning.to_string();
    }
    if final_reasoning.is_empty() {
        return streamed.to_string();
    }
    if final_reasoning.chars().count() > streamed.chars().count() {
        return final_reasoning.to_string();
    }

    streamed.to_string()
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let mut out = trimmed
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use kerux_core::config::AppConfig;

    use super::*;

    #[test]
    fn landing_starts_in_command_mode() {
        let state = AppState::new(AppConfig::default(), String::new(), false);
        assert_eq!(state.ui.view, ViewMode::Landing);
        assert_eq!(state.ui.input_mode, InputMode::Command);
    }

    #[test]
    fn run_failures_stay_in_tui_state() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), false);
        state.begin_run("hello".to_string());
        state.fail_run("api failed".to_string());

        assert_eq!(state.session.status, "Run failed");
        assert_eq!(state.session.error.as_deref(), Some("api failed"));
        assert!(!state.session.running);
        assert_eq!(state.ui.input_mode, InputMode::Prompt);
        assert_eq!(
            state
                .ui
                .footer_notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some("follow-up prompt ready")
        );
        assert_eq!(
            state.session.activity.last().map(|item| item.body.as_str()),
            Some("api failed")
        );
    }

    #[test]
    fn failed_run_tool_sublines_do_not_leak_into_next_run() {
        use kerux_core::tools::ToolResult;
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), false);
        state.begin_run("first try".to_string());
        state.apply_agent_event(AgentEvent::ToolStart {
            call_id: "call-1".to_string(),
            name: "read_file".to_string(),
            arguments: "src/main.rs".to_string(),
        });
        state.apply_agent_event(AgentEvent::ToolComplete {
            result: ToolResult {
                tool_call_id: "call-1".to_string(),
                success: true,
                content: "ok".to_string(),
                error: None,
            },
        });
        state.fail_run("api failed".to_string());

        state.begin_run("second try".to_string());
        state.apply_agent_event(AgentEvent::Done {
            message: Message::assistant("recovered"),
        });

        let assistant = state
            .session
            .transcript
            .iter()
            .rev()
            .find(|entry| entry.role == "Assistant")
            .expect("assistant entry exists");
        assert!(
            assistant.tools.is_empty(),
            "stale tool sub-lines leaked into the next run: {:?}",
            assistant.tools
        );
        assert!(state.session.pending_tools.is_empty());
    }

    #[test]
    fn operational_events_use_activity_and_short_notice() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.record_app_event(
            "Skill reload failed",
            "Skill reload failed: bad manifest",
            Tone::Error,
            Some("skill reload failed".to_string()),
        );

        assert_eq!(state.session.status, "Idle");
        assert_eq!(state.session.error, None);
        assert_eq!(
            state
                .ui
                .footer_notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some("skill reload failed")
        );
        assert_eq!(
            state
                .session
                .activity
                .last()
                .map(|item| item.label.as_str()),
            Some("Skill reload failed")
        );
    }

    #[test]
    fn completed_runs_return_to_prompt_mode_for_follow_up() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), false);
        state.begin_run("hello".to_string());
        state.apply_agent_event(AgentEvent::Done {
            message: Message::assistant("all done"),
        });

        assert_eq!(state.session.status, "Completed");
        assert_eq!(state.ui.input_mode, InputMode::Prompt);
        assert_eq!(
            state
                .ui
                .footer_notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some("follow-up prompt ready")
        );
    }

    #[test]
    fn telemetry_updates_context_and_cost() {
        let mut config = AppConfig::default();
        config.telemetry.currency = "EUR".to_string();
        config.telemetry.input_cost_per_million = 2.0;
        config.telemetry.output_cost_per_million = 6.0;
        let mut state = AppState::new(config, String::new(), false);

        state.apply_agent_event(AgentEvent::Telemetry {
            telemetry: AgentTelemetry {
                prompt_tokens: 1_000,
                completion_tokens: 500,
                total_tokens: 1_500,
                context_window: 10_000,
                compacted: true,
                estimated: false,
                billable: true,
                tokens_per_second: Some(35.0),
                estimated_cost_usd: Some(0.005),
                turns_completed: 1,
                context_window_usage_pct: Some(15.0),
                cached_prompt_tokens: 200,
            },
        });

        assert_eq!(state.session.telemetry.total_tokens, 1_500);
        assert!(state.session.telemetry.compacted);
        assert!((state.session.telemetry.total_cost - 0.005).abs() < f64::EPSILON);
    }

    #[test]
    fn shell_runs_use_shell_transcript_role() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_shell_run("echo hello".to_string());

        assert_eq!(state.session.status, "Running shell command");
        assert_eq!(state.session.active_query, "$ echo hello");
        assert_eq!(state.session.transcript[0].role, "Shell");
    }

    #[test]
    fn begin_run_auto_names_session_from_prompt() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);

        state.begin_run(
            "Fix the TUI scroll bug. Add tests for Windows terminals. Ignore unrelated files."
                .to_string(),
        );

        assert_eq!(
            state.session.title,
            "Fix the TUI scroll bug. Add tests for Windows terminals."
        );
    }

    #[test]
    fn session_title_is_normalized_and_truncated() {
        let title = derive_session_title(
            "  Please   refactor the terminal user interface to support a much cleaner layout with panels, tabs, and responsive behavior across small screens  ",
        );

        assert_eq!(
            title,
            "Please refactor the terminal user interface to support a much..."
        );
    }

    #[test]
    fn follow_up_prompt_does_not_rename_existing_session() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_run("Implement mouse scrolling".to_string());
        state.apply_agent_event(AgentEvent::Done {
            message: Message::assistant("done"),
        });

        state.begin_run("Also add tests".to_string());

        assert_eq!(state.session.title, "Implement mouse scrolling");
    }

    #[test]
    fn session_title_ignores_common_abbreviation_punctuation() {
        let title = derive_session_title("Use e.g. Ratatui widgets. Add tests. Keep it small.");

        assert_eq!(title, "Use e.g. Ratatui widgets. Add tests.");
    }

    #[test]
    fn completed_runs_prefer_longer_final_message_over_partial_stream() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), false);
        state.begin_run("hello".to_string());
        state.session.streaming_response = "Apa yang bis".to_string();
        state.apply_agent_event(AgentEvent::Done {
            message: Message::assistant("Apa yang bisa saya bantu?"),
        });

        assert_eq!(
            state.session.final_message.as_deref(),
            Some("Apa yang bisa saya bantu?")
        );
        assert_eq!(
            state
                .session
                .transcript
                .last()
                .map(|entry| entry.content.as_str()),
            Some("Apa yang bisa saya bantu?")
        );
    }

    #[test]
    fn completed_runs_prefer_longer_final_reasoning_over_partial_stream() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), false);
        state.begin_run("hello".to_string());
        state.session.reasoning = "Let me use the echo tool to simply echo the".to_string();
        state.apply_agent_event(AgentEvent::Done {
            message: Message::assistant("done")
                .with_reasoning("Let me use the echo tool to simply echo the greeting back."),
        });

        assert_eq!(
            state.session.reasoning,
            "Let me use the echo tool to simply echo the greeting back."
        );
    }

    #[test]
    fn conversation_scroll_moves_and_resets() {
        let mut state = AppState::new(AppConfig::default(), String::new(), true);
        state.scroll_conversation_down(10);
        state.scroll_conversation_up(3);
        assert!(!state.follow_conversation_tail());
        assert_eq!(state.conversation_scroll(), 7);

        state.begin_run("hello".to_string());
        assert_eq!(state.conversation_scroll(), 0);
        assert!(state.follow_conversation_tail());

        state.scroll_conversation_down(5);
        state.apply_agent_event(AgentEvent::Done {
            message: Message::assistant("done"),
        });
        assert_eq!(state.conversation_scroll(), 0);
        assert!(state.follow_conversation_tail());
    }

    #[test]
    fn prompt_history_cycles_latest_first_and_restores_draft() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);

        state.begin_run("first".to_string());
        state.begin_run("second".to_string());
        state.ui.prompt_input = "draft".to_string();

        state.prompt_history_previous();
        assert_eq!(state.ui.prompt_input, "second");

        state.prompt_history_previous();
        assert_eq!(state.ui.prompt_input, "first");

        state.prompt_history_next();
        assert_eq!(state.ui.prompt_input, "second");

        state.prompt_history_next();
        assert_eq!(state.ui.prompt_input, "draft");
    }

    #[test]
    fn prompt_history_deduplicates_consecutive_entries() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);

        state.begin_run("repeat".to_string());
        state.begin_run("repeat".to_string());

        assert_eq!(state.ui.prompt_history, vec!["repeat".to_string()]);
    }

    #[test]
    fn apply_agent_event_thinking() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_run("test".to_string());
        state.apply_agent_event(AgentEvent::Thinking {
            content: "Pondering...".to_string(),
        });

        assert_eq!(state.session.status, "Pondering...");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Thinking");
        assert_eq!(last_activity.body, "Pondering...");
        assert_eq!(last_activity.tone, Tone::Info);
    }

    #[test]
    fn apply_agent_event_reasoning() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_run("test".to_string());
        state.apply_agent_event(AgentEvent::Reasoning {
            text: "Step 1...".to_string(),
        });
        state.apply_agent_event(AgentEvent::Reasoning {
            text: " Step 2...".to_string(),
        });

        assert_eq!(state.session.reasoning, "Step 1... Step 2...");
        assert_eq!(state.session.status, "Streaming reasoning");
    }

    #[test]
    fn apply_agent_event_tool_start() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_run("test".to_string());
        state.apply_agent_event(AgentEvent::ToolStart {
            call_id: "call_1".to_string(),
            name: "calculator".to_string(),
            arguments: "1+1".to_string(),
        });

        assert_eq!(state.session.status, "Running calculator");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool calculator");
        assert_eq!(last_activity.body, "1+1");
        assert_eq!(last_activity.tone, Tone::Warning);
    }

    #[test]
    fn apply_agent_event_tool_complete() {
        use kerux_core::tools::ToolResult;
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_run("test".to_string());

        // Success case
        state.apply_agent_event(AgentEvent::ToolComplete {
            result: ToolResult {
                tool_call_id: "1".to_string(),
                success: true,
                content: "2".to_string(),
                error: None,
            },
        });
        assert_eq!(state.session.status, "Tool completed");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool complete");
        assert_eq!(last_activity.body, "2");
        assert_eq!(last_activity.tone, Tone::Success);

        // Failure case
        state.apply_agent_event(AgentEvent::ToolComplete {
            result: ToolResult {
                tool_call_id: "2".to_string(),
                success: false,
                content: "syntax error".to_string(),
                error: Some("error".to_string()),
            },
        });
        assert_eq!(state.session.status, "Tool completed");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool complete");
        assert_eq!(last_activity.body, "syntax error");
        assert_eq!(last_activity.tone, Tone::Error);
    }

    #[test]
    fn apply_agent_event_tool_error() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_run("test".to_string());
        state.apply_agent_event(AgentEvent::ToolError {
            name: "calculator".to_string(),
            error: "timeout".to_string(),
        });

        assert_eq!(state.session.status, "calculator failed");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool calculator");
        assert_eq!(last_activity.body, "timeout");
        assert_eq!(last_activity.tone, Tone::Error);
    }

    #[test]
    fn apply_agent_event_content_and_iteration() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.begin_run("test".to_string());

        state.apply_agent_event(AgentEvent::Content {
            text: "Hello".to_string(),
        });
        state.apply_agent_event(AgentEvent::Content {
            text: " World".to_string(),
        });
        assert_eq!(state.session.streaming_response, "Hello World");
        assert_eq!(state.session.status, "Streaming response");

        state.apply_agent_event(AgentEvent::IterationComplete { iteration: 5 });
        assert_eq!(state.session.current_iteration, 5);
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Iteration 5");
        assert_eq!(last_activity.body, "Agent loop step finished.");
        assert_eq!(last_activity.tone, Tone::Info);
    }

    #[test]
    fn truncate_handles_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("  hello  ", 10), "hello");
    }

    #[test]
    fn truncate_handles_exact_length_strings() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("  hello  ", 5), "hello");
    }

    #[test]
    fn truncate_adds_ellipsis_when_too_long() {
        assert_eq!(truncate("hello world", 8), "hello...");
        assert_eq!(truncate("  hello world  ", 8), "hello...");
    }

    #[test]
    fn truncate_handles_very_short_max_chars() {
        assert_eq!(truncate("hello", 2), "...");
        assert_eq!(truncate("hello", 0), "...");
    }

    #[test]
    fn truncate_handles_multibyte_chars_correctly() {
        assert_eq!(truncate("👋🌍", 2), "👋🌍");
        assert_eq!(truncate("👋🌍👋🌍", 3), "...");
        assert_eq!(truncate("👋🌍👋🌍", 4), "👋🌍👋🌍");
        assert_eq!(truncate("👋🌍👋🌍👋🌍", 4), "👋...");
    }
}

#[cfg(test)]
mod apply_agent_event_tests {
    use super::*;
    use kerux_core::config::AppConfig;
    use kerux_core::tools::ToolResult;

    #[test]
    fn thinking_updates_status_and_activity() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.apply_agent_event(AgentEvent::Thinking {
            content: "analyzing problem".to_string(),
        });

        assert_eq!(state.session.status, "analyzing problem");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Thinking");
        assert_eq!(last_activity.body, "analyzing problem");
        assert_eq!(last_activity.tone, Tone::Info);
    }

    #[test]
    fn reasoning_appends_to_reasoning_and_updates_status() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.session.reasoning = "initial ".to_string();
        state.apply_agent_event(AgentEvent::Reasoning {
            text: "thoughts".to_string(),
        });

        assert_eq!(state.session.reasoning, "initial thoughts");
        assert_eq!(state.session.status, "Streaming reasoning");
    }

    #[test]
    fn tool_start_updates_status_and_activity() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.apply_agent_event(AgentEvent::ToolStart {
            call_id: "call_1".to_string(),
            name: "calculator".to_string(),
            arguments: "1 + 1".to_string(),
        });

        assert_eq!(state.session.status, "Running calculator");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool calculator");
        assert_eq!(last_activity.body, "1 + 1");
        assert_eq!(last_activity.tone, Tone::Warning);
    }

    #[test]
    fn tool_complete_success_updates_status_and_activity() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.apply_agent_event(AgentEvent::ToolComplete {
            result: ToolResult {
                tool_call_id: "call_1".to_string(),
                success: true,
                content: "result: 2".to_string(),
                error: None,
            },
        });

        assert_eq!(state.session.status, "Tool completed");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool complete");
        assert_eq!(last_activity.body, "result: 2");
        assert_eq!(last_activity.tone, Tone::Success);
    }

    #[test]
    fn tool_complete_error_updates_status_and_activity() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.apply_agent_event(AgentEvent::ToolComplete {
            result: ToolResult {
                tool_call_id: "call_2".to_string(),
                success: false,
                content: "syntax error".to_string(),
                error: Some("bad input".to_string()),
            },
        });

        assert_eq!(state.session.status, "Tool completed");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool complete");
        assert_eq!(last_activity.body, "syntax error");
        assert_eq!(last_activity.tone, Tone::Error);
    }

    #[test]
    fn tool_error_updates_status_and_activity() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.apply_agent_event(AgentEvent::ToolError {
            name: "calculator".to_string(),
            error: "timeout".to_string(),
        });

        assert_eq!(state.session.status, "calculator failed");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Tool calculator");
        assert_eq!(last_activity.body, "timeout");
        assert_eq!(last_activity.tone, Tone::Error);
    }

    #[test]
    fn content_appends_to_streaming_response() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.session.streaming_response = "Hello ".to_string();
        state.apply_agent_event(AgentEvent::Content {
            text: "World".to_string(),
        });

        assert_eq!(state.session.streaming_response, "Hello World");
        assert_eq!(state.session.status, "Streaming response");
    }

    #[test]
    fn iteration_complete_updates_iteration_and_activity() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.apply_agent_event(AgentEvent::IterationComplete { iteration: 3 });

        assert_eq!(state.session.current_iteration, 3);
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Iteration 3");
        assert_eq!(last_activity.body, "Agent loop step finished.");
        assert_eq!(last_activity.tone, Tone::Info);
    }

    #[test]
    fn error_sets_session_error_and_activity() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.apply_agent_event(AgentEvent::Error {
            error: "fatal error".to_string(),
        });

        assert_eq!(state.session.error, Some("fatal error".to_string()));
        assert_eq!(state.session.status, "Errored");
        let last_activity = state.session.activity.last().unwrap();
        assert_eq!(last_activity.label, "Error");
        assert_eq!(last_activity.body, "fatal error");
        assert_eq!(last_activity.tone, Tone::Error);
    }
}
