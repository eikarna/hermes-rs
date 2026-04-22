use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::state::{
    ActivePanel, AppState, InputMode, LayoutMode, McpServerItem, SkillItem, Tone, TranscriptEntry,
};

const BG: Color = Color::Black;
const PANEL: Color = Color::Rgb(26, 24, 22);
const PANEL_ALT: Color = Color::Rgb(18, 17, 15);
const ACCENT: Color = Color::Rgb(232, 165, 54);
const TEXT: Color = Color::Rgb(230, 228, 222);
const MUTED: Color = Color::Rgb(134, 132, 126);
const HELP: Color = Color::Rgb(188, 184, 176);
const SUCCESS: Color = Color::Rgb(115, 185, 115);
const ERROR: Color = Color::Rgb(220, 98, 87);
const WARN: Color = Color::Rgb(208, 170, 82);
const CONSTRAINED_WIDTH: u16 = 65;
const CONSTRAINED_HEIGHT: u16 = 20;
const DESKTOP_WIDTH: u16 = 120;
const DESKTOP_HEIGHT: u16 = 24;

#[derive(Clone, Copy)]
struct DesktopWorkspaceLayout {
    header: Rect,
    conversation: Rect,
    panel: Rect,
    reasoning: Rect,
    activity: Rect,
    footer: Rect,
}

#[derive(Clone, Copy)]
struct MediumWorkspaceLayout {
    header: Rect,
    conversation: Rect,
    tabs: Rect,
    panel: Rect,
    reasoning: Rect,
    footer: Rect,
}

#[derive(Clone, Copy)]
struct CompactWorkspaceLayout {
    header: Rect,
    tabs: Rect,
    content: Rect,
    footer: Rect,
}

#[derive(Clone, Copy)]
struct ConstrainedWorkspaceLayout {
    header: Rect,
    content: Rect,
    footer: Rect,
    popup: Rect,
}

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(background_fill(area, BG), area);

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
    if is_constrained(area) {
        render_landing_constrained(frame, state, area);
        return;
    }

    if area.width < 100 {
        render_landing_compact(frame, state, area);
        return;
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(18),
            Constraint::Max(6),
            Constraint::Max(8),
            Constraint::Min(3),
            Constraint::Max(2),
        ])
        .split(area);

    let title = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            state.persistent.config.tui.landing_title.clone(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "prompt-first terminal agent",
            Style::default().fg(MUTED),
        )),
    ]))
    .style(Style::default().bg(BG))
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
                truncate_display(&state.persistent.behavior.model, area.width as usize / 2),
                Style::default().fg(TEXT),
            ),
        ]),
    ]))
    .block(prompt_block)
    .wrap(Wrap { trim: true });
    let prompt_area = centered_rect_percent(area, 52, 40, 100, 8);
    frame.render_widget(prompt, prompt_area);

    let footer_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(vertical[4]);

    let footer = Paragraph::new(Text::from(vec![Line::from(vec![
        Span::styled(
            "tab",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" panels   ", Style::default().fg(HELP)),
        Span::styled(
            "i",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" prompt   ", Style::default().fg(HELP)),
        Span::styled(
            "enter",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" run", Style::default().fg(HELP)),
    ])]))
    .style(Style::default().bg(BG))
    .alignment(Alignment::Right);
    frame.render_widget(footer, footer_row[1]);

    let status = Paragraph::new(Line::from(vec![Span::styled(
        status_summary(state),
        Style::default()
            .fg(status_color(state))
            .add_modifier(Modifier::BOLD),
    )]))
    .style(Style::default().bg(BG))
    .alignment(Alignment::Left);
    frame.render_widget(status, footer_row[0]);
}

fn render_landing_compact(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let is_portrait_like = area.width < 56;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if is_portrait_like {
            [
                Constraint::Max(4),
                Constraint::Min(7),
                Constraint::Max(4),
                Constraint::Min(1),
                Constraint::Max(2),
            ]
        } else {
            [
                Constraint::Max(4),
                Constraint::Min(7),
                Constraint::Max(3),
                Constraint::Min(1),
                Constraint::Max(2),
            ]
        })
        .split(area);

    let title = Paragraph::new(Text::from(vec![
        Line::from(Span::styled(
            state.persistent.config.tui.landing_title.clone(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "prompt-first terminal agent",
            Style::default().fg(MUTED),
        )),
    ]))
    .style(Style::default().bg(BG))
    .alignment(Alignment::Center);
    frame.render_widget(title, outer[0]);

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
                truncate_display(&state.persistent.behavior.model, area.width as usize / 2),
                Style::default().fg(TEXT),
            ),
        ]),
    ]))
    .block(panel_block("Prompt"))
    .wrap(Wrap { trim: true });
    frame.render_widget(prompt, outer[1]);

    let controls = if is_portrait_like {
        vec![
            Line::from(vec![
                keycap("q"),
                label(" quit   "),
                keycap("i"),
                label(" prompt"),
            ]),
            Line::from(vec![
                keycap("Enter"),
                label(" run   "),
                keycap("Tab"),
                label(" panels"),
            ]),
        ]
    } else {
        vec![Line::from(vec![
            keycap("q"),
            label(" quit   "),
            keycap("i"),
            label(" prompt   "),
            keycap("Enter"),
            label(" run   "),
            keycap("Tab"),
            label(" panels"),
        ])]
    };
    let help = Paragraph::new(Text::from(controls))
        .style(Style::default().bg(BG))
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true });
    frame.render_widget(help, outer[2]);

    let status = Paragraph::new(Line::from(vec![Span::styled(
        status_summary(state),
        Style::default()
            .fg(status_color(state))
            .add_modifier(Modifier::BOLD),
    )]))
    .style(Style::default().bg(BG))
    .alignment(Alignment::Left);
    frame.render_widget(status, outer[4]);
}

fn render_landing_constrained(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Max(2),
            Constraint::Min(4),
            Constraint::Max(2),
            Constraint::Max(1),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            truncate_display(
                &state.persistent.config.tui.landing_title,
                area.width as usize,
            ),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  prompt-first", Style::default().fg(MUTED)),
    ]))
    .style(Style::default().bg(BG))
    .alignment(Alignment::Left)
    .wrap(Wrap { trim: true });
    frame.render_widget(title, outer[0]);

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
        Line::from(vec![
            Span::styled(
                "Plan",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                truncate_display(
                    &state.persistent.behavior.model,
                    area.width.saturating_sub(7) as usize,
                ),
                Style::default().fg(TEXT),
            ),
        ]),
    ]))
    .block(panel_block("Prompt"))
    .wrap(Wrap { trim: true });
    frame.render_widget(prompt, outer[1]);

    let help = Paragraph::new(Text::from(vec![Line::from(vec![
        keycap("q"),
        label(" quit  "),
        keycap("i"),
        label(" prompt  "),
        keycap("Enter"),
        label(" run  "),
        keycap("Tab"),
        label(" panels"),
    ])]))
    .style(Style::default().bg(BG))
    .wrap(Wrap { trim: true });
    frame.render_widget(help, outer[2]);

    let status = Paragraph::new(Line::from(Span::styled(
        status_summary(state),
        Style::default()
            .fg(status_color(state))
            .add_modifier(Modifier::BOLD),
    )))
    .style(Style::default().bg(BG))
    .wrap(Wrap { trim: true });
    frame.render_widget(status, outer[3]);
}

fn render_workspace(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    if is_constrained(area) {
        render_workspace_constrained(frame, state, area);
        return;
    }

    match responsive_workspace_mode(state.ui.layout, area) {
        LayoutMode::Wide => render_workspace_wide(frame, state, area),
        LayoutMode::Medium => render_workspace_medium(frame, state, area),
        LayoutMode::Compact => render_workspace_compact(frame, state, area),
    }
}

fn render_workspace_wide(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let layout = build_desktop_layout(area);
    frame.render_widget(header_widget(state), layout.header);
    render_conversation_widget(frame, state, layout.conversation);
    frame.render_widget(panel_widget(state), layout.panel);
    frame.render_widget(reasoning_widget(state), layout.reasoning);
    frame.render_widget(activity_widget(state), layout.activity);
    frame.render_widget(footer_widget(state), layout.footer);
}

fn render_workspace_medium(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let layout = build_medium_layout(area);
    frame.render_widget(header_widget(state), layout.header);
    render_conversation_widget(frame, state, layout.conversation);
    frame.render_widget(panel_tabs(state), layout.tabs);
    frame.render_widget(panel_widget(state), layout.panel);
    frame.render_widget(reasoning_widget(state), layout.reasoning);
    frame.render_widget(footer_widget(state), layout.footer);
}

fn render_workspace_compact(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let layout = build_compact_layout(area);
    frame.render_widget(header_widget(state), layout.header);
    frame.render_widget(panel_tabs(state), layout.tabs);
    if state.ui.active_panel == ActivePanel::Session {
        render_session_compact_widget(frame, state, layout.content);
    } else {
        frame.render_widget(panel_widget(state), layout.content);
    }
    frame.render_widget(footer_widget(state), layout.footer);
}

fn render_workspace_constrained(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let layout = build_constrained_layout(area);
    frame.render_widget(constrained_header_widget(state, area.width), layout.header);
    render_conversation_widget(frame, state, layout.content);

    if state.ui.active_panel != ActivePanel::Session {
        frame.render_widget(Clear, layout.popup);
        frame.render_widget(panel_widget(state), layout.popup);
    }

    frame.render_widget(constrained_footer_widget(state, area.width), layout.footer);
}

fn is_constrained(area: Rect) -> bool {
    area.width < CONSTRAINED_WIDTH || area.height < CONSTRAINED_HEIGHT
}

fn responsive_workspace_mode(mode: LayoutMode, area: Rect) -> LayoutMode {
    if area.height < DESKTOP_HEIGHT {
        return LayoutMode::Compact;
    }
    if mode == LayoutMode::Wide && area.width < DESKTOP_WIDTH {
        return LayoutMode::Medium;
    }
    mode
}

fn build_desktop_layout(area: Rect) -> DesktopWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(4),
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

    DesktopWorkspaceLayout {
        header: outer[0],
        conversation: body[0],
        panel: right[0],
        reasoning: right[1],
        activity: right[2],
        footer: outer[2],
    }
}

fn build_medium_layout(area: Rect) -> MediumWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Ratio(1, 2),
            Constraint::Length(3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 6),
            Constraint::Length(4),
        ])
        .split(area);

    MediumWorkspaceLayout {
        header: outer[0],
        conversation: outer[1],
        tabs: outer[2],
        panel: outer[3],
        reasoning: outer[4],
        footer: outer[5],
    }
}

fn build_compact_layout(area: Rect) -> CompactWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(4),
        ])
        .split(area);

    CompactWorkspaceLayout {
        header: outer[0],
        tabs: outer[1],
        content: outer[2],
        footer: outer[3],
    }
}

fn build_constrained_layout(area: Rect) -> ConstrainedWorkspaceLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Max(1), Constraint::Min(3), Constraint::Max(2)])
        .split(area);

    ConstrainedWorkspaceLayout {
        header: outer[0],
        content: outer[1],
        footer: outer[2],
        popup: centered_rect_percent(outer[1], 92, 88, 80, 16),
    }
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
                truncate_display(&state.persistent.behavior.model, 42),
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

fn constrained_header_widget(state: &AppState, width: u16) -> Paragraph<'_> {
    let model_width = width.saturating_sub(18) as usize;
    Paragraph::new(Line::from(vec![
        Span::styled(
            " hermes ",
            Style::default()
                .fg(BG)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            truncate_display(&state.persistent.behavior.model, model_width),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(state.ui.active_panel.title(), Style::default().fg(ACCENT)),
    ]))
    .style(Style::default().bg(PANEL_ALT))
    .wrap(Wrap { trim: true })
}

fn render_conversation_widget(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let lines = conversation_lines(state);
    let block = panel_block("Conversation");
    let inner = block.inner(area);
    let max_scroll = max_wrapped_scroll(&lines, inner.width, inner.height);
    let scroll = if state.follow_conversation_tail() {
        max_scroll
    } else {
        state.conversation_scroll().min(max_scroll)
    };

    let widget = Paragraph::new(Text::from(lines))
        .block(block)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn conversation_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for entry in state.session.transcript.iter().rev().take(8).rev() {
        lines.push(role_line(entry));
        lines.extend(render_message_body(&entry.content));
        lines.push(Line::from(""));
    }

    if state.session.running {
        lines.push(Line::from(vec![
            Span::styled(
                "Assistant",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                if state.persistent.behavior.stream {
                    "(streaming)"
                } else {
                    "(responding)"
                },
                Style::default().fg(MUTED),
            ),
        ]));
        lines.extend(render_message_body(
            if state.session.streaming_response.is_empty() {
                "Waiting for visible assistant output..."
            } else {
                &state.session.streaming_response
            },
        ));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No conversation yet. Press i and type a prompt.",
            Style::default().fg(MUTED),
        )));
    }

    lines
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
        .wrap(Wrap { trim: true })
}

fn activity_widget(state: &AppState) -> Paragraph<'_> {
    let mut lines = state
        .session
        .activity
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|entry| {
            let color = tone_color(entry.tone);
            Line::from(vec![
                Span::styled("• ", Style::default().fg(color)),
                Span::styled(
                    entry.label.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(": ", Style::default().fg(MUTED)),
                Span::styled(truncate_text(&entry.body, 72), Style::default().fg(TEXT)),
            ])
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No activity yet.",
            Style::default().fg(MUTED),
        )));
    }

    Paragraph::new(Text::from(lines))
        .block(panel_block("Activity"))
        .wrap(Wrap { trim: true })
}

fn footer_widget(state: &AppState) -> Paragraph<'_> {
    let prompt = if state.ui.prompt_input.is_empty() {
        state.persistent.config.tui.prompt_placeholder.clone()
    } else {
        state.ui.prompt_input.clone()
    };
    let mut footer_line = vec![Span::styled(
        state.ui.footer_help.clone(),
        Style::default().fg(MUTED),
    )];
    let status = status_summary(state);
    if let Some(notice) = &state.ui.footer_notice {
        if !notice.text.eq_ignore_ascii_case(&status) {
            footer_line.push(Span::raw("  "));
            footer_line.push(Span::styled("•", Style::default().fg(MUTED)));
            footer_line.push(Span::raw(" "));
            footer_line.push(Span::styled(
                notice.text.clone(),
                Style::default()
                    .fg(tone_color(notice.tone))
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    footer_line.push(Span::raw("  "));
    footer_line.push(Span::styled(
        status,
        Style::default()
            .fg(status_color(state))
            .add_modifier(Modifier::BOLD),
    ));

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
                truncate_display(&prompt, 180),
                Style::default().fg(if state.ui.prompt_input.is_empty() {
                    MUTED
                } else {
                    TEXT
                }),
            ),
        ]),
        Line::from(footer_line),
    ]))
    .block(panel_block("Input"))
    .wrap(Wrap { trim: true })
}

fn constrained_footer_widget(state: &AppState, width: u16) -> Paragraph<'_> {
    let prompt = if state.ui.prompt_input.is_empty() {
        state.persistent.config.tui.prompt_placeholder.clone()
    } else {
        state.ui.prompt_input.clone()
    };
    let status = status_summary(state);
    let prompt_width = width.saturating_sub(status.len() as u16 + 12) as usize;

    Paragraph::new(Text::from(vec![Line::from(vec![
        Span::styled(
            if matches!(state.ui.input_mode, InputMode::Prompt) {
                "PROMPT "
            } else {
                "CMD "
            },
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_display(&prompt, prompt_width),
            Style::default().fg(if state.ui.prompt_input.is_empty() {
                MUTED
            } else {
                TEXT
            }),
        ),
        Span::raw(" "),
        Span::styled(
            status,
            Style::default()
                .fg(status_color(state))
                .add_modifier(Modifier::BOLD),
        ),
    ])]))
    .style(Style::default().bg(PANEL_ALT))
    .wrap(Wrap { trim: true })
}

fn status_summary(state: &AppState) -> String {
    if state.session.running {
        format!(
            "step {} of {}",
            state.session.current_iteration.max(1),
            state.persistent.behavior.max_iterations
        )
    } else if state.session.error.is_some() {
        "run failed".to_string()
    } else if state.session.final_message.is_some() {
        "completed".to_string()
    } else {
        "idle".to_string()
    }
}

fn status_color(state: &AppState) -> Color {
    if state.session.running {
        ACCENT
    } else if state.session.error.is_some() {
        ERROR
    } else if state.session.final_message.is_some() {
        SUCCESS
    } else {
        HELP
    }
}

fn tone_color(tone: Tone) -> Color {
    match tone {
        Tone::Info => ACCENT,
        Tone::Success => SUCCESS,
        Tone::Warning => WARN,
        Tone::Error => ERROR,
    }
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
                truncate_display(&state.session.active_query, 120),
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
    .wrap(Wrap { trim: true })
}

fn render_session_compact_widget(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "Conversation",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )));
    for entry in state.session.transcript.iter().rev().take(4).rev() {
        lines.push(role_line(entry));
        lines.extend(render_message_body(&entry.content));
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

    let block = panel_block("Session");
    let inner = block.inner(area);
    let max_scroll = max_wrapped_scroll(&lines, inner.width, inner.height);
    let scroll = if state.follow_conversation_tail() {
        max_scroll
    } else {
        state.conversation_scroll().min(max_scroll)
    };

    let widget = Paragraph::new(Text::from(lines))
        .block(block)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn max_wrapped_scroll(lines: &[Line<'_>], width: u16, height: u16) -> u16 {
    if width == 0 || height == 0 {
        return 0;
    }

    let available_width = usize::from(width.max(1));
    let total_rows = lines
        .iter()
        .map(|line| {
            let width = line.width();
            width.max(1).div_ceil(available_width)
        })
        .sum::<usize>();
    total_rows
        .saturating_sub(usize::from(height))
        .min(usize::from(u16::MAX)) as u16
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
                    Span::styled(
                        truncate_display(&server.endpoint, 96),
                        Style::default().fg(TEXT),
                    ),
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
        .wrap(Wrap { trim: true })
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
            Span::styled(
                truncate_display(&skills_root, 96),
                Style::default().fg(TEXT),
            ),
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
                    Span::styled(
                        truncate_display(&skill.description, 140),
                        Style::default().fg(MUTED),
                    ),
                ]));
            }
        }
    }

    Paragraph::new(Text::from(lines))
        .block(panel_block("Skills"))
        .wrap(Wrap { trim: true })
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
            Span::styled(truncate_display(value, 120), Style::default().fg(TEXT)),
        ]));
    }

    Paragraph::new(Text::from(lines))
        .block(panel_block("Behavior"))
        .wrap(Wrap { trim: true })
}

fn render_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    help: &str,
    fields: &[crate::tui::forms::FormField],
    selected: usize,
) {
    let modal_area = centered_rect_percent(area, 88, 82, 92, 16);
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
                    truncate_display(title, 48),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .style(Style::default().bg(PANEL)),
        )
        .wrap(Wrap { trim: true });

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

fn keycap(text: &str) -> Span<'static> {
    Span::styled(
        text.to_string(),
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
    )
}

fn label(text: &str) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(HELP))
}

fn background_fill(area: Rect, color: Color) -> Paragraph<'static> {
    let blank_line = " ".repeat(area.width as usize);
    let lines = (0..area.height)
        .map(|_| Line::raw(blank_line.clone()))
        .collect::<Vec<_>>();

    Paragraph::new(Text::from(lines)).style(Style::default().bg(color))
}

fn render_message_body(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![Line::from("")];
    }

    let lines = text
        .split('\n')
        .map(|raw_line| raw_line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let mut rendered = Vec::new();
    let mut table_mode = false;

    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            table_mode = false;
            rendered.push(Line::from(""));
            continue;
        }

        if is_table_alignment_row(line) {
            table_mode = true;
            continue;
        }

        let next_is_alignment = lines
            .get(index + 1)
            .is_some_and(|next| is_table_alignment_row(next));

        if looks_like_table_row(line) {
            rendered.push(render_table_row(line, next_is_alignment));
            table_mode = next_is_alignment || table_mode;
            continue;
        }

        if table_mode {
            if looks_like_table_row(line) {
                rendered.push(render_table_row(line, false));
                continue;
            }
            table_mode = false;
        }

        rendered.push(render_markdown_line(line));
    }

    rendered
}

fn render_markdown_line(line: &str) -> Line<'static> {
    if line.trim().is_empty() {
        return Line::from("");
    }

    if is_horizontal_rule(line) {
        return Line::from(Span::styled("─".repeat(24), Style::default().fg(MUTED)));
    }

    if let Some(rest) = heading_text(line) {
        return Line::from(vec![Span::styled(
            rest.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )]);
    }

    if let Some(rest) = blockquote_text(line) {
        let mut spans = vec![Span::styled("▎ ", Style::default().fg(MUTED))];
        spans.extend(parse_inline_markdown(rest, InlineStyle::Quote));
        return Line::from(spans);
    }

    if let Some((checked, body)) = task_list_item(line) {
        return Line::from(vec![
            Span::styled(
                if checked { "☑ " } else { "☐ " },
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(body.to_string(), Style::default().fg(TEXT)),
        ]);
    }

    if let Some((prefix, body)) = list_prefix(line) {
        let mut spans = vec![Span::styled(
            prefix.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )];
        spans.extend(parse_inline_markdown(body, InlineStyle::Default));
        return Line::from(spans);
    }

    Line::from(parse_inline_markdown(line, InlineStyle::Default))
}

fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let marker_count = trimmed.chars().take_while(|ch| *ch == '#').count();
    if marker_count == 0 {
        return None;
    }

    let remainder = trimmed[marker_count..].trim_start();
    if remainder.is_empty() {
        None
    } else {
        Some(remainder)
    }
}

fn list_prefix(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Some(("• ", rest));
    }

    let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits > 0 && trimmed[digits..].starts_with(". ") {
        let prefix = &trimmed[..digits + 2];
        let body = &trimmed[digits + 2..];
        return Some((prefix, body));
    }

    None
}

#[derive(Clone, Copy)]
enum InlineStyle {
    Default,
    Quote,
    TableHeader,
    TableCell,
}

fn parse_inline_markdown(line: &str, style: InlineStyle) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = line;

    while !remaining.is_empty() {
        let bold_index = remaining.find("**").map(|index| (index, "**"));
        let italic_index = remaining.find('*').map(|index| (index, "*"));
        let code_index = remaining.find('`').map(|index| (index, "`"));
        let marker = [bold_index, italic_index, code_index]
            .into_iter()
            .flatten()
            .filter(|(index, token)| *token != "*" || !remaining[*index..].starts_with("**"))
            .min_by_key(|(index, _)| *index);

        let Some((index, token)) = marker else {
            spans.push(plain_span(remaining, style));
            break;
        };

        if index > 0 {
            spans.push(plain_span(&remaining[..index], style));
        }

        if token == "**" {
            let inner = &remaining[index + 2..];
            if let Some(end) = inner.find("**") {
                spans.push(Span::styled(
                    inner[..end].to_string(),
                    base_style(style).add_modifier(Modifier::BOLD),
                ));
                remaining = &inner[end + 2..];
                continue;
            }
        } else if token == "*" {
            let inner = &remaining[index + 1..];
            if let Some(end) = inner.find('*') {
                spans.push(Span::styled(
                    inner[..end].to_string(),
                    base_style(style).add_modifier(Modifier::ITALIC),
                ));
                remaining = &inner[end + 1..];
                continue;
            }
        } else {
            let inner = &remaining[index + 1..];
            if let Some(end) = inner.find('`') {
                spans.push(Span::styled(
                    inner[..end].to_string(),
                    Style::default()
                        .fg(ACCENT)
                        .bg(PANEL_ALT)
                        .add_modifier(Modifier::BOLD),
                ));
                remaining = &inner[end + 1..];
                continue;
            }
        }

        spans.push(plain_span(token, style));
        remaining = &remaining[index + token.len()..];
    }

    spans
}

fn plain_span(text: &str, style: InlineStyle) -> Span<'static> {
    Span::styled(text.to_string(), base_style(style))
}

fn base_style(style: InlineStyle) -> Style {
    match style {
        InlineStyle::Default => Style::default().fg(TEXT),
        InlineStyle::Quote => Style::default().fg(HELP),
        InlineStyle::TableHeader => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        InlineStyle::TableCell => Style::default().fg(TEXT),
    }
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && trimmed
            .chars()
            .all(|ch| ch == '-' || ch == '*' || ch == '_')
}

fn blockquote_text(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("> ").map(str::trim_start)
}

fn task_list_item(line: &str) -> Option<(bool, &str)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed
        .strip_prefix("- [x] ")
        .or_else(|| trimmed.strip_prefix("* [x] "))
    {
        return Some((true, rest));
    }
    if let Some(rest) = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("* [ ] "))
    {
        return Some((false, rest));
    }
    None
}

fn looks_like_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.matches('|').count() >= 2 && !is_horizontal_rule(trimmed)
}

fn is_table_alignment_row(line: &str) -> bool {
    let trimmed = line.trim();
    looks_like_table_row(trimmed)
        && trimmed.trim_matches('|').split('|').all(|cell| {
            let cell = cell.trim();
            !cell.is_empty() && cell.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
        })
}

fn render_table_row(line: &str, header: bool) -> Line<'static> {
    let cells = line
        .trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>();

    let mut spans = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(MUTED)));
        }
        spans.extend(parse_inline_markdown(
            cell,
            if header {
                InlineStyle::TableHeader
            } else {
                InlineStyle::TableCell
            },
        ));
    }

    Line::from(spans)
}

fn centered_rect_percent(
    area: Rect,
    width_percent: u16,
    height_percent: u16,
    max_width: u16,
    max_height: u16,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }

    let width = area
        .width
        .saturating_mul(width_percent.min(100))
        .saturating_div(100)
        .min(max_width)
        .max(1)
        .min(area.width.max(1));
    let height = area
        .height
        .saturating_mul(height_percent.min(100))
        .saturating_div(100)
        .min(max_height)
        .max(1)
        .min(area.height.max(1));

    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn truncate_text(text: &str, max: usize) -> String {
    truncate_display(text, max)
}

fn truncate_display(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let mut width = 0;
    let mut value = String::new();
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width - 3 {
            break;
        }
        width += ch_width;
        value.push(ch);
    }
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
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

    fn buffer(state: &AppState, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn landing_layout_renders_prompt_first_shell() {
        let state = AppState::new(AppConfig::default(), String::new(), false);
        let text = buffer_text(&state, 120, 36);
        assert!(text.contains("HERMES"));
        assert!(text.contains("prompt"));
    }

    #[test]
    fn compact_portrait_landing_wraps_controls() {
        let state = AppState::new(AppConfig::default(), String::new(), false);
        let text = buffer_text(&state, 40, 28);
        assert!(text.contains("quit"));
        assert!(text.contains("prompt"));
        assert!(text.contains("run"));
        assert!(text.contains("idle"));
    }

    #[test]
    fn compact_landscape_landing_keeps_controls_below_prompt() {
        let state = AppState::new(AppConfig::default(), String::new(), false);
        let text = buffer_text(&state, 72, 24);
        assert!(text.contains("panels"));
        assert!(text.contains("quit"));
        assert!(!text.contains("iter 0/"));
    }

    #[test]
    fn wide_landing_renders_single_idle_and_visible_helpers() {
        let state = AppState::new(AppConfig::default(), String::new(), false);
        let buffer = buffer(&state, 120, 36);
        let footer_row = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .find(|line| line.contains("idle") && line.contains("tab"))
            .unwrap();
        let idle_index = footer_row.find("idle").unwrap();
        let tab_index = footer_row.find("tab").unwrap();

        assert_eq!(footer_row.matches("idle").count(), 1);
        assert!(tab_index > idle_index);
        assert!(footer_row.contains("enter"));
    }

    #[test]
    fn landing_canvas_uses_black_background() {
        let state = AppState::new(AppConfig::default(), String::new(), false);
        let buffer = buffer(&state, 120, 36);
        assert!(buffer
            .content
            .iter()
            .all(|cell| cell.bg == BG || cell.bg == PANEL));
    }

    #[test]
    fn activity_panel_renders_run_failures() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(160);
        state.fail_run("OpenAI 401 unauthorized".to_string());

        let text = buffer_text(&state, 160, 40);
        assert!(text.contains("Activity"));
        assert!(text.contains("Run failed: OpenAI 401 unauthorized"));
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
    fn desktop_workspace_uses_split_layout_at_120_by_30() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(120);

        let text = buffer_text(&state, 120, 30);

        assert!(text.contains("Conversation"));
        assert!(text.contains("Reasoning"));
        assert!(text.contains("Activity"));
        assert!(!text.contains("Panels"));
    }

    #[test]
    fn medium_workspace_renders_tabs() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(110);
        let text = buffer_text(&state, 110, 36);
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
        assert!(text.contains("idle"));
        assert!(!text.contains("iter 0/"));
    }

    #[test]
    fn constrained_workspace_prioritizes_conversation_at_40_by_15() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(40);

        let text = buffer_text(&state, 40, 15);

        assert!(text.contains("Conversation"));
        assert!(text.contains("PROMPT"));
        assert!(text.contains("idle"));
        assert!(!text.contains("Panels"));
        assert!(!text.contains("Reasoning"));
        assert!(!text.contains("Activity"));
    }

    #[test]
    fn constrained_workspace_shows_secondary_panel_as_popup() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.ui.active_panel = ActivePanel::Mcp;
        state.set_layout_for_width(40);

        let text = buffer_text(&state, 40, 15);

        assert!(text.contains("MCP"));
        assert!(text.contains("No MCP servers configured."));
        assert!(!text.contains("Panels"));
    }

    #[test]
    fn renderer_handles_extreme_small_viewports() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;

        for (width, height) in [(1, 1), (20, 8), (40, 15), (64, 19)] {
            state.set_layout_for_width(width);
            let _ = buffer_text(&state, width, height);
        }
    }

    #[test]
    fn conversation_preserves_line_breaks() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(160);
        state.session.transcript.push(TranscriptEntry {
            role: "Assistant",
            content: "Line one\n\nLine two\n- item".to_string(),
        });

        let text = buffer_text(&state, 160, 40);
        assert!(text.contains("Line one"));
        assert!(text.contains("Line two"));
        assert!(text.contains("• item"));
    }

    #[test]
    fn conversation_strips_common_markdown_markers() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(160);
        state.session.transcript.push(TranscriptEntry {
            role: "Assistant",
            content: "## Heading\n**Bold** and `code`".to_string(),
        });

        let text = buffer_text(&state, 160, 40);
        assert!(text.contains("Heading"));
        assert!(text.contains("Bold and code"));
        assert!(!text.contains("## Heading"));
        assert!(!text.contains("**Bold**"));
        assert!(!text.contains("`code`"));
    }

    #[test]
    fn conversation_scroll_reveals_later_lines() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(160);
        state.session.transcript.push(TranscriptEntry {
            role: "Assistant",
            content: "entry 01\nentry 02\nentry 03\nentry 04\nentry 05\nentry 06\nentry 07\nentry 08\nentry 09\nentry 10\nentry 11\nentry 12".to_string(),
        });
        state.scroll_conversation_down(6);

        let text = buffer_text(&state, 160, 22);
        assert!(text.contains("entry 08"));
        assert!(!text.contains("entry 01"));
    }

    #[test]
    fn conversation_formats_tables_quotes_and_rules() {
        let mut state = AppState::new(AppConfig::default(), "hello".to_string(), true);
        state.ui.view = ViewMode::Workspace;
        state.set_layout_for_width(160);
        state.session.transcript.push(TranscriptEntry {
            role: "Assistant",
            content:
                "---\n> quoted line\n| Tool | Use |\n| --- | --- |\n| echo | test |\n- [x] done"
                    .to_string(),
        });

        let text = buffer_text(&state, 160, 40);
        assert!(text.contains("quoted line"));
        assert!(text.contains("Tool │ Use"));
        assert!(text.contains("echo │ test"));
        assert!(text.contains("☑ done"));
        assert!(!text.contains("| --- | --- |"));
    }
}
