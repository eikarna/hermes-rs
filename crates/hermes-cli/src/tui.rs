use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use hermes_core::agent::AgentEvent;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Terminal,
};

pub struct LiveRunTui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: LiveRunState,
}

#[derive(Debug, Clone)]
struct LiveRunState {
    model: String,
    query: String,
    max_iterations: usize,
    current_iteration: usize,
    assistant: String,
    reasoning: String,
    activity: Vec<ActivityItem>,
    status: String,
    final_response: Option<String>,
    final_reasoning: Option<String>,
    error: Option<String>,
    show_thinking: bool,
    show_tool_calls: bool,
    show_iterations: bool,
}

#[derive(Debug, Clone)]
struct ActivityItem {
    label: String,
    body: String,
    tone: Tone,
}

#[derive(Debug, Clone, Copy)]
enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

impl LiveRunTui {
    pub fn enter(
        model: impl Into<String>,
        query: impl Into<String>,
        max_iterations: usize,
        show_thinking: bool,
        show_tool_calls: bool,
        show_iterations: bool,
    ) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Self {
            terminal,
            state: LiveRunState {
                model: model.into(),
                query: query.into(),
                max_iterations,
                current_iteration: 0,
                assistant: String::new(),
                reasoning: String::new(),
                activity: vec![ActivityItem {
                    label: "Ready".to_string(),
                    body: "Waiting for first model event.".to_string(),
                    tone: Tone::Info,
                }],
                status: "Starting run".to_string(),
                final_response: None,
                final_reasoning: None,
                error: None,
                show_thinking,
                show_tool_calls,
                show_iterations,
            },
        })
    }

    pub fn apply_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Thinking { content } => {
                self.state.status = content.clone();
                if self.state.show_thinking {
                    self.push_activity("Thinking", content, Tone::Info);
                }
            }
            AgentEvent::Reasoning { text } => {
                if self.state.show_thinking {
                    self.state.reasoning.push_str(text);
                    self.state.status = "Streaming reasoning".to_string();
                }
            }
            AgentEvent::ToolStart { name, arguments } => {
                if self.state.show_tool_calls {
                    self.push_activity(
                        format!("Tool {}", name),
                        format!("Args: {}", truncate(arguments, 120)),
                        Tone::Warning,
                    );
                }
                self.state.status = format!("Running tool `{}`", name);
            }
            AgentEvent::ToolComplete { result } => {
                if self.state.show_tool_calls {
                    self.push_activity(
                        format!("Tool {}", result.tool_call_id),
                        truncate(&result.content, 180),
                        if result.success {
                            Tone::Success
                        } else {
                            Tone::Error
                        },
                    );
                }
                self.state.status = "Tool finished".to_string();
            }
            AgentEvent::ToolError { name, error } => {
                if self.state.show_tool_calls {
                    self.push_activity(format!("Tool {}", name), error.clone(), Tone::Error);
                }
                self.state.status = format!("Tool `{}` failed", name);
            }
            AgentEvent::Content { text } => {
                self.state.assistant.push_str(text);
                self.state.status = "Streaming response".to_string();
            }
            AgentEvent::Done { message } => {
                self.state.assistant = message.content.clone();
                self.state.final_response = Some(message.content.clone());
                if let Some(reasoning) = &message.reasoning {
                    self.state.final_reasoning = Some(reasoning.clone());
                    if self.state.show_thinking && self.state.reasoning.is_empty() {
                        self.state.reasoning = reasoning.clone();
                    }
                }
                self.push_activity("Done", "Model completed response.", Tone::Success);
                self.state.status = "Completed".to_string();
            }
            AgentEvent::IterationComplete { iteration } => {
                self.state.current_iteration = *iteration;
                if self.state.show_iterations {
                    self.push_activity(
                        format!("Iteration {}", iteration),
                        "Agent loop step finished.".to_string(),
                        Tone::Info,
                    );
                }
                self.state.status = format!(
                    "Iteration {}/{} complete",
                    iteration, self.state.max_iterations
                );
            }
            AgentEvent::Error { error } => {
                self.state.error = Some(error.clone());
                self.push_activity("Error", error.clone(), Tone::Error);
                self.state.status = "Errored".to_string();
            }
        }
    }

    pub fn draw(&mut self) -> Result<()> {
        let state = self.state.clone();
        self.terminal.draw(|frame| {
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(12),
                    Constraint::Length(5),
                ])
                .split(frame.area());

            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(outer[1]);

            let side = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(body[1]);

            frame.render_widget(Self::header_widget(&state), outer[0]);
            frame.render_widget(Self::conversation_widget(&state), body[0]);
            frame.render_widget(Self::reasoning_widget(&state), side[0]);
            frame.render_widget(Self::activity_widget(&state), side[1]);
            frame.render_widget(Self::status_widget(&state), outer[2]);
        })?;

        Ok(())
    }

    pub fn exit(mut self) -> Result<()> {
        self.restore()?;
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    fn header_widget(state: &LiveRunState) -> Paragraph<'_> {
        let title = Line::from(vec![
            Span::styled(
                " Hermes-RS ",
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
            Span::raw(" "),
            Span::styled(
                format!("model {}", state.model),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let meta = Line::from(vec![
            Span::styled("query ", Style::default().fg(Color::Gray)),
            Span::raw(truncate(&state.query, 80)),
        ]);

        Paragraph::new(Text::from(vec![title, meta]))
            .block(Block::default().borders(Borders::ALL).title("Session"))
    }

    fn conversation_widget(state: &LiveRunState) -> Paragraph<'_> {
        let assistant = if state.assistant.trim().is_empty() {
            "Waiting for visible assistant output...".to_string()
        } else {
            state.assistant.clone()
        };

        let text = Text::from(vec![
            Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Green)),
                Span::styled("User", Style::default().add_modifier(Modifier::BOLD)),
            ]),
            Line::raw(state.query.clone()),
            Line::raw(""),
            Line::from(vec![
                Span::styled("◉ ", Style::default().fg(Color::Cyan)),
                Span::styled("Assistant", Style::default().add_modifier(Modifier::BOLD)),
            ]),
            Line::raw(assistant),
        ]);

        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Conversation"))
            .wrap(Wrap { trim: false })
            .scroll(Self::scroll_for_text(
                state.assistant.lines().count() + state.query.lines().count() + 6,
            ))
    }

    fn reasoning_widget(state: &LiveRunState) -> Paragraph<'_> {
        let reasoning = if state.show_thinking {
            if state.reasoning.trim().is_empty() {
                "No structured reasoning received.".to_string()
            } else {
                state.reasoning.clone()
            }
        } else {
            "Reasoning display disabled by config.".to_string()
        };

        Paragraph::new(reasoning)
            .block(Block::default().borders(Borders::ALL).title("Reasoning"))
            .wrap(Wrap { trim: false })
            .scroll(Self::scroll_for_text(state.reasoning.lines().count() + 2))
    }

    fn activity_widget(state: &LiveRunState) -> List<'_> {
        let items: Vec<ListItem<'_>> = state
            .activity
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|entry| {
                let color = match entry.tone {
                    Tone::Info => Color::Blue,
                    Tone::Success => Color::Green,
                    Tone::Warning => Color::Yellow,
                    Tone::Error => Color::Red,
                };

                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled("▸ ", Style::default().fg(color)),
                        Span::styled(
                            entry.label.clone(),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::raw(truncate(&entry.body, 140)),
                ])
            })
            .collect();

        List::new(items).block(Block::default().borders(Borders::ALL).title("Activity"))
    }

    fn status_widget(state: &LiveRunState) -> Paragraph<'_> {
        let iteration = if state.current_iteration == 0 {
            format!("0/{}", state.max_iterations)
        } else {
            format!("{}/{}", state.current_iteration, state.max_iterations)
        };
        let reasoning_chars = state.reasoning.chars().count();
        let assistant_chars = state.assistant.chars().count();

        let mut lines = vec![
            Line::from(vec![
                Span::styled("Status ", Style::default().fg(Color::Gray)),
                Span::raw(state.status.clone()),
            ]),
            Line::from(vec![
                Span::styled("Iteration ", Style::default().fg(Color::Gray)),
                Span::raw(iteration),
                Span::raw("    "),
                Span::styled("Assistant ", Style::default().fg(Color::Gray)),
                Span::raw(assistant_chars.to_string()),
                Span::raw(" chars"),
                Span::raw("    "),
                Span::styled("Reasoning ", Style::default().fg(Color::Gray)),
                Span::raw(reasoning_chars.to_string()),
                Span::raw(" chars"),
            ]),
        ];

        if let Some(error) = &state.error {
            lines.push(Line::from(vec![
                Span::styled("Error ", Style::default().fg(Color::Red)),
                Span::raw(truncate(error, 120)),
            ]));
        }

        Paragraph::new(Text::from(lines))
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true })
    }

    fn push_activity(&mut self, label: impl Into<String>, body: impl Into<String>, tone: Tone) {
        self.state.activity.push(ActivityItem {
            label: label.into(),
            body: body.into(),
            tone,
        });
    }

    fn scroll_for_text(line_count: usize) -> (u16, u16) {
        let offset = line_count.saturating_sub(18);
        (offset as u16, 0)
    }
}

impl Drop for LiveRunTui {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let mut out = trimmed
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        out.push('…');
        out
    }
}
