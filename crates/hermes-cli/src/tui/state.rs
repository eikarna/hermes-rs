use std::path::PathBuf;

use hermes_core::agent::AgentEvent;
use hermes_core::client::Message;
use hermes_core::config::{AppConfig, BehaviorSettings, McpTransportKind};

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
}

#[derive(Debug, Clone)]
pub struct SessionState {
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
}

impl SessionState {
    pub fn new(max_iterations: usize) -> Self {
        Self {
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
                prompt_history: Vec::new(),
                prompt_history_index: None,
                prompt_history_draft: None,
                selected_mcp: 0,
                selected_skill: 0,
                selected_behavior: 0,
                footer_help: "tab panels  w/m/s/b views  ctrl+l new session  q quit".to_string(),
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
            AgentEvent::ToolStart { name, arguments } => {
                self.push_activity(
                    format!("Tool {}", name),
                    truncate(&arguments, 140),
                    Tone::Warning,
                );
                self.session.status = format!("Running {}", name);
            }
            AgentEvent::ToolComplete { result } => {
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
        self.session.error = None;
        self.session.final_message = None;
        self.session.active_query = query.clone();
        self.session.streaming_response.clear();
        self.session.reasoning.clear();
        self.session.current_iteration = 0;
        self.session.status = "Requesting model response".to_string();
        self.session.transcript.push(TranscriptEntry {
            role: "User",
            content: query,
        });
    }

    pub fn fail_run(&mut self, error: String) {
        self.session.running = false;
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
        self.session.transcript.push(TranscriptEntry {
            role: "Assistant",
            content,
        });
        if !reasoning.is_empty() {
            self.session.reasoning = reasoning;
        }
        self.push_activity("Done", "Response finished.", Tone::Success);
        self.set_footer_notice("follow-up prompt ready", Tone::Success);
    }
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
    use hermes_core::config::AppConfig;

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
