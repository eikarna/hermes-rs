use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::tui::state::{
    ActivePanel, AppState, InputMode, LayoutMode, McpServerItem, SkillItem, Tone, TranscriptEntry,
};

const BG: Color = Color::Black;
const PANEL: Color = Color::Rgb(26, 24, 22);
const PANEL_ALT: Color = Color::Rgb(18, 17, 15);
const ACCENT: Color = Color::Rgb(232, 165, 54);
const TEXT: Color = Color::Rgb(230, 228, 222);
const MUTED: Color = Color::Rgb(134, 132, 126);
const SUCCESS: Color = Color::Rgb(115, 185, 115);
const ERROR: Color = Color::Rgb(220, 98, 87);
const WARN: Color = Color::Rgb(208, 170, 82);

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    match state.ui.view {
        crate::tui::state::ViewMode::Landing => render_landing(frame, state, area),
        crate::tui::state::ViewMode::Workspace => render_workspace(frame, state, area),
    }

    if let Some(modal) = &state.ui.modal {
        render_modal(
            frame,
            area,
            modal.title(),
            modal.help(),
            &modal.form().fields,
            modal.form().selected,
        );
    }
}

fn render_landing(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(18),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    let title = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            state.persistent.config.tui.landing_title.clone(),
            Style::default()
                .fg(TEXT)
                .add_modifier(Modifier::BOLD | Modifier::RAPID_BLINK),
        )),
        Line::from(Span::styled(
            "prompt-first terminal agent",
            Style::default().fg(MUTED),
        )),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(title, vertical[1]);

    let prompt_block = Block::default()
        .borders(Borders::LEFT | Borders::BOTTOM)
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL))
        .title(Span::styled(" prompt ", Style::default().fg(ACCENT)));
    let prompt_text = if state.ui.prompt_input.is_empty() {
        state.persistent.config.tui.prompt_placeholder.clone()
    } else {
        state.ui.prompt_input.clone()
    };
    let prompt = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            prompt_text,
            Style::default()
                .fg(if state.ui.prompt_input.is_empty() {
                    MUTED
                } else {
                    TEXT
                })
                .add_modifier(if matches!(state.ui.input_mode, InputMode::Prompt) {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Plan",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" · "),
            Span::styled(
                state.persistent.behavior.model.clone(),
                Style::default().fg(TEXT),
            ),
        ]),
    ]))
    .block(prompt_block)
    .wrap(Wrap { trim: false });
    let prompt_area = centered_rect(52, 8, area);
    frame.render_widget(prompt, prompt_area);

    let footer = Paragraph::new(Text::from(vec![Line::from(vec![
        Span::styled(
            "tab",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" panels   ", Style::default().fg(MUTED)),
        Span::styled("i", Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
        Span::styled(" prompt   ", Style::default().fg(MUTED)),
        Span::styled(
            "enter",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" run", Style::default().fg(MUTED)),
    ])]))
    .alignment(Alignment::Right);
    frame.render_widget(footer, vertical[3]);

    let status = Paragraph::new(Line::from(vec![
        Span::styled(state.session.status.clone(), Style::default().fg(MUTED)),
        Span::raw("  "),
        Span::styled(
            if state.session.running {
                "live"
            } else {
                "idle"
            },
            Style::default().fg(ACCENT),
        ),
    ]))
    .alignment(Alignment::Left);
    frame.render_widget(status, vertical[4]);
}

fn render_workspace(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    match state.ui.layout {
        LayoutMode::Wide => render_workspace_wide(frame, state, area),
        LayoutMode::Medium => render_workspace_medium(frame, state, area),
        LayoutMode::Compact => render_workspace_compact(frame, state, area),
    }
}

fn render_workspace_wide(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(5),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(outer[1]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(42),
            Constraint::Percentage(33),
            Constraint::Percentage(25),
        ])
        .split(body[1]);

    frame.render_widget(header_widget(state), outer[0]);
    frame.render_widget(conversation_widget(state), body[0]);
    frame.render_widget(panel_widget(state), right[0]);
    frame.render_widget(reasoning_widget(state), right[1]);
    frame.render_widget(activity_widget(state), right[2]);
    frame.render_widget(footer_widget(state), outer[2]);
}

fn render_workspace_medium(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Percentage(48),
            Constraint::Length(3),
            Constraint::Percentage(27),
            Constraint::Percentage(12),
            Constraint::Length(5),
        ])
        .split(area);

    frame.render_widget(header_widget(state), outer[0]);
    frame.render_widget(conversation_widget(state), outer[1]);
    frame.render_widget(panel_tabs(state), outer[2]);
    frame.render_widget(panel_widget(state), outer[3]);
    frame.render_widget(reasoning_widget(state), outer[4]);
    frame.render_widget(footer_widget(state), outer[5]);
}

fn render_workspace_compact(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(5),
        ])
        .split(area);

    frame.render_widget(header_widget(state), outer[0]);
    frame.render_widget(panel_tabs(state), outer[1]);
    frame.render_widget(
        match state.ui.active_panel {
            ActivePanel::Session => session_compact_widget(state),
            _ => panel_widget(state),
        },
        outer[2],
    );
    frame.render_widget(footer_widget(state), outer[3]);
}

fn header_widget(state: &AppState) -> Paragraph<'_> {
    Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled(
                " hermes ",
                Style::default()
                    .fg(BG)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                state.persistent.behavior.model.clone(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(state.ui.active_panel.title(), Style::default().fg(ACCENT)),
        ]),
        Line::from(vec![
            Span::styled("status ", Style::default().fg(MUTED)),
            Span::styled(state.session.status.clone(), Style::default().fg(TEXT)),
        ]),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(MUTED))
            .style(Style::default().bg(PANEL_ALT)),
    )
}

fn conversation_widget(state: &AppState) -> Paragraph<'_> {
    let mut lines = Vec::new();
    for entry in state.session.transcript.iter().rev().take(8).rev() {
        lines.push(role_line(entry));
        lines.push(Line::from(entry.content.clone()));
        lines.push(Line::from(""));
    }

    if state.session.running {
        lines.push(Line::from(vec![
            Span::styled(
                "Assistant",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled("(streaming)", Style::default().fg(MUTED)),
        ]));
        lines.push(Line::from(if state.session.streaming_response.is_empty() {
            "Waiting for visible assistant output...".to_string()
        } else {
            state.session.streaming_response.clone()
        }));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No conversation yet. Press i and type a prompt.",
            Style::default().fg(MUTED),
        )));
    }

    Paragraph::new(Text::from(lines))
        .block(panel_block("Conversation"))
        .wrap(Wrap { trim: false })
}

fn reasoning_widget(state: &AppState) -> Paragraph<'_> {
    let body = if state.session.reasoning.trim().is_empty() {
        if state.persistent.behavior.show_reasoning {
            "Reasoning pane waiting for structured thinking."
        } else {
            "Reasoning display disabled."
        }
    } else {
        &state.session.reasoning
    };

    Paragraph::new(body.to_string())
        .block(panel_block("Reasoning"))
        .wrap(Wrap { trim: false })
}

fn activity_widget(state: &AppState) -> List<'_> {
    let items = state
        .session
        .activity
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|entry| {
            let color = match entry.tone {
                Tone::Info => ACCENT,
                Tone::Success => SUCCESS,
                Tone::Warning => WARN,
                Tone::Error => ERROR,
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled("> ", Style::default().fg(color)),
                    Span::styled(
                        entry.label.clone(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(entry.body.clone(), Style::default().fg(TEXT))),
            ])
        })
        .collect::<Vec<_>>();

    List::new(items).block(panel_block("Activity"))
}

fn footer_widget(state: &AppState) -> Paragraph<'_> {
    let prompt = if state.ui.prompt_input.is_empty() {
        state.persistent.config.tui.prompt_placeholder.clone()
    } else {
        state.ui.prompt_input.clone()
    };
    Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled(
                if matches!(state.ui.input_mode, InputMode::Prompt) {
                    "PROMPT"
                } else {
                    "COMMAND"
                },
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                prompt,
                Style::default().fg(if state.ui.prompt_input.is_empty() {
                    MUTED
                } else {
                    TEXT
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(state.ui.footer.clone(), Style::default().fg(MUTED)),
            Span::raw("  "),
            Span::styled(
                format!(
                    "iter {}/{}",
                    state.session.current_iteration, state.persistent.behavior.max_iterations
                ),
                Style::default().fg(TEXT),
            ),
        ]),
    ]))
    .block(panel_block("Input"))
    .wrap(Wrap { trim: true })
}

fn panel_tabs(state: &AppState) -> Tabs<'_> {
    let titles = ActivePanel::all()
        .iter()
        .map(|panel| Line::from(panel.title()))
        .collect::<Vec<_>>();
    let index = ActivePanel::all()
        .iter()
        .position(|panel| *panel == state.ui.active_panel)
        .unwrap_or(0);

    Tabs::new(titles)
        .select(index)
        .block(panel_block("Panels"))
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .style(Style::default().fg(MUTED))
}

fn panel_widget(state: &AppState) -> Paragraph<'_> {
    match state.ui.active_panel {
        ActivePanel::Session => session_summary_widget(state),
        ActivePanel::Mcp => mcp_widget(&state.persistent.mcp_servers, state.ui.selected_mcp),
        ActivePanel::Skills => skills_widget(
            &state.persistent.skills,
            state.ui.selected_skill,
            state.persistent.skills_root.display().to_string(),
        ),
        ActivePanel::Behavior => behavior_widget(state),
    }
}

fn session_summary_widget(state: &AppState) -> Paragraph<'_> {
    Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled("query ", Style::default().fg(MUTED)),
            Span::styled(
                state.session.active_query.clone(),
                Style::default().fg(TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("responses ", Style::default().fg(MUTED)),
            Span::styled(
                state
                    .session
                    .transcript
                    .iter()
                    .filter(|entry| entry.role == "Assistant")
                    .count()
                    .to_string(),
                Style::default().fg(TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("reasoning chars ", Style::default().fg(MUTED)),
            Span::styled(
                state.session.reasoning.chars().count().to_string(),
                Style::default().fg(TEXT),
            ),
        ]),
        Line::from(vec![
            Span::styled("needs rebuild ", Style::default().fg(MUTED)),
            Span::styled(
                state.persistent.needs_rebuild.to_string(),
                Style::default().fg(TEXT),
            ),
        ]),
    ]))
    .block(panel_block("Session"))
}

fn session_compact_widget(state: &AppState) -> Paragraph<'_> {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "Conversation",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )));
    for entry in state.session.transcript.iter().rev().take(4).rev() {
        lines.push(role_line(entry));
        lines.push(Line::from(entry.content.clone()));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Reasoning",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(truncate_text(
        if state.session.reasoning.is_empty() {
            "Waiting for reasoning..."
        } else {
            &state.session.reasoning
        },
        300,
    )));

    Paragraph::new(Text::from(lines))
        .block(panel_block("Session"))
        .wrap(Wrap { trim: false })
}

fn mcp_widget(servers: &[McpServerItem], selected: usize) -> Paragraph<'_> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "a",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" add  ", Style::default().fg(MUTED)),
            Span::styled(
                "d",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" remove", Style::default().fg(MUTED)),
        ]),
        Line::from(""),
    ];

    if servers.is_empty() {
        lines.push(Line::from(Span::styled(
            "No MCP servers configured.",
            Style::default().fg(MUTED),
        )));
    } else {
        for (index, server) in servers.iter().enumerate() {
            lines.push(mcp_line(server, index == selected));
            if index == selected {
                lines.push(Line::from(vec![
                    Span::styled("    endpoint ", Style::default().fg(MUTED)),
                    Span::styled(server.endpoint.clone(), Style::default().fg(TEXT)),
                    Span::raw("  "),
                    Span::styled(
                        if server.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        Style::default().fg(if server.enabled { SUCCESS } else { WARN }),
                    ),
                ]));
            }
        }
    }

    Paragraph::new(Text::from(lines))
        .block(panel_block("MCP Servers"))
        .wrap(Wrap { trim: false })
}

fn skills_widget(skills: &[SkillItem], selected: usize, skills_root: String) -> Paragraph<'_> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "n",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" new  ", Style::default().fg(MUTED)),
            Span::styled(
                "r",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" reload  ", Style::default().fg(MUTED)),
            Span::styled(
                "d",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" delete", Style::default().fg(MUTED)),
        ]),
        Line::from(vec![
            Span::styled("root ", Style::default().fg(MUTED)),
            Span::styled(skills_root, Style::default().fg(TEXT)),
        ]),
        Line::from(""),
    ];

    if skills.is_empty() {
        lines.push(Line::from(Span::styled(
            "No skills loaded.",
            Style::default().fg(MUTED),
        )));
    } else {
        for (index, skill) in skills.iter().enumerate() {
            lines.push(skill_line(skill, index == selected));
            if index == selected {
                lines.push(Line::from(vec![
                    Span::styled("    ", Style::default().fg(MUTED)),
                    Span::styled(skill.description.clone(), Style::default().fg(MUTED)),
                ]));
            }
        }
    }

    Paragraph::new(Text::from(lines))
        .block(panel_block("Skills"))
        .wrap(Wrap { trim: false })
}

fn behavior_widget(state: &AppState) -> Paragraph<'_> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "e",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" edit selected  ", Style::default().fg(MUTED)),
            Span::styled(
                "space",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" toggle bool", Style::default().fg(MUTED)),
        ]),
        Line::from(""),
    ];

    for (index, (key, value)) in state.behavior_rows().iter().enumerate() {
        let selected = index == state.ui.selected_behavior;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "> " } else { "  " },
                Style::default().fg(if selected { ACCENT } else { MUTED }),
            ),
            Span::styled(
                format!("{key}: "),
                Style::default()
                    .fg(if selected { TEXT } else { MUTED })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(value.clone(), Style::default().fg(TEXT)),
        ]));
    }

    Paragraph::new(Text::from(lines))
        .block(panel_block("Behavior"))
        .wrap(Wrap { trim: false })
}

fn render_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    help: &str,
    fields: &[crate::tui::forms::FormField],
    selected: usize,
) {
    let modal_area = centered_rect(68, 14, area);
    frame.render_widget(Clear, modal_area);

    let mut lines = vec![
        Line::from(Span::styled(help, Style::default().fg(MUTED))),
        Line::from(""),
    ];
    for (index, field) in fields.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                if index == selected { "> " } else { "  " },
                Style::default().fg(if index == selected { ACCENT } else { MUTED }),
            ),
            Span::styled(
                format!("{}: ", field.label),
                Style::default()
                    .fg(if index == selected { TEXT } else { MUTED })
                    .add_modifier(if index == selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(field.display_value(), Style::default().fg(TEXT)),
        ]));
    }

    let modal = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title(Span::styled(
                    title,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .style(Style::default().bg(PANEL)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(modal, modal_area);
}

fn role_line(entry: &TranscriptEntry) -> Line<'static> {
    let color = if entry.role == "User" {
        SUCCESS
    } else {
        ACCENT
    };
    Line::from(vec![
        Span::styled(
            entry.role.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ])
}

fn mcp_line(server: &McpServerItem, selected: bool) -> Line<'static> {
    let status = if server.connected {
        "connected"
    } else {
        "offline"
    };
    Line::from(vec![
        Span::styled(
            if selected { "> " } else { "  " },
            Style::default().fg(if selected { ACCENT } else { MUTED }),
        ),
        Span::styled(
            server.name.clone(),
            Style::default().fg(TEXT).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::raw(" "),
        Span::styled(
            format!("[{:?}]", server.transport).to_lowercase(),
            Style::default().fg(MUTED),
        ),
        Span::raw(" "),
        Span::styled(
            status,
            Style::default().fg(if server.connected { SUCCESS } else { WARN }),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{} tools", server.tool_count),
            Style::default().fg(MUTED),
        ),
    ])
}

fn skill_line(skill: &SkillItem, selected: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if selected { "> " } else { "  " },
            Style::default().fg(if selected { ACCENT } else { MUTED }),
        ),
        Span::styled(
            skill.name.clone(),
            Style::default().fg(TEXT).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::raw(" "),
        Span::styled(skill.version.clone(), Style::default().fg(MUTED)),
        Span::raw(" "),
        Span::styled(
            if skill.available { "ready" } else { "blocked" },
            Style::default().fg(if skill.available { SUCCESS } else { WARN }),
        ),
    ])
}

fn panel_block(title: &str) -> Block<'static> {
    Block::default()
        .title(Span::styled(title.to_string(), Style::default().fg(ACCENT)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(MUTED))
        .style(Style::default().bg(PANEL))
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(height),
            Constraint::Min(1),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut value = text.chars().take(max.saturating_sub(3)).collect::<String>();
        value.push_str("...");
        value
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use hermes_core::config::AppConfig;

    use super::*;
    use crate::tui::state::{AppState, ViewMode};

    fn buffer_text(state: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn landing_layout_renders_prompt_first_shell() {
        let state = AppState::new(AppConfig::default(), String::new(), false);
        let text = buffer_text(&state, 120, 36);
        assert!(text.contains("HERMES"));
        assert!(text.contains("prompt"));
    }

    #[test]
    fn wide_workspace_renders_all_major_panels() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(160);
        let text = buffer_text(&state, 160, 40);
        assert!(text.contains("Conversation"));
        assert!(text.contains("Reasoning"));
        assert!(text.contains("Activity"));
    }

    #[test]
    fn medium_workspace_renders_tabs() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(120);
        let text = buffer_text(&state, 120, 36);
        assert!(text.contains("Panels"));
        assert!(text.contains("Session"));
    }

    #[test]
    fn compact_workspace_renders_single_panel_mode() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(80);
        let text = buffer_text(&state, 80, 32);
        assert!(text.contains("Session"));
        assert!(text.contains("Input"));
    }
}
