use hermes_core::agent::AgentEvent;

use crate::tui::forms::Modal;
use crate::tui::state::{ActivePanel, AppState, InputMode, McpServerItem, SkillItem, ViewMode};

#[derive(Debug, Clone)]
pub enum Action {
    SetView(ViewMode),
    SetActivePanel(ActivePanel),
    CyclePanelForward,
    CyclePanelBackward,
    SetInputMode(InputMode),
    AppendPrompt(char),
    PromptBackspace,
    ClearPrompt,
    OpenModal(Modal),
    CloseModal,
    UpdateModal(Modal),
    StartRun(String),
    AgentEvent(AgentEvent),
    RunFailed(String),
    ClearSession,
    SetFooter(String),
    SyncMcp(Vec<McpServerItem>),
    SyncSkills(Vec<SkillItem>),
    SelectNext,
    SelectPrevious,
    Quit,
}

impl AppState {
    pub fn reduce(&mut self, action: Action) {
        match action {
            Action::SetView(view) => self.ui.view = view,
            Action::SetActivePanel(panel) => self.ui.active_panel = panel,
            Action::CyclePanelForward => self.ui.active_panel = self.ui.active_panel.next(),
            Action::CyclePanelBackward => self.ui.active_panel = self.ui.active_panel.previous(),
            Action::SetInputMode(mode) => self.ui.input_mode = mode,
            Action::AppendPrompt(ch) => self.ui.prompt_input.push(ch),
            Action::PromptBackspace => {
                self.ui.prompt_input.pop();
            }
            Action::ClearPrompt => self.ui.prompt_input.clear(),
            Action::OpenModal(modal) => self.ui.modal = Some(modal),
            Action::CloseModal => self.ui.modal = None,
            Action::UpdateModal(modal) => self.ui.modal = Some(modal),
            Action::StartRun(query) => self.begin_run(query),
            Action::AgentEvent(event) => self.apply_agent_event(event),
            Action::RunFailed(error) => self.fail_run(error),
            Action::ClearSession => self.clear_session(),
            Action::SetFooter(footer) => self.ui.footer = footer,
            Action::SyncMcp(servers) => {
                self.persistent.mcp_servers = servers;
                self.ui.selected_mcp = self
                    .ui
                    .selected_mcp
                    .min(self.persistent.mcp_servers.len().saturating_sub(1));
            }
            Action::SyncSkills(skills) => {
                self.persistent.skills = skills;
                self.ui.selected_skill = self
                    .ui
                    .selected_skill
                    .min(self.persistent.skills.len().saturating_sub(1));
            }
            Action::SelectNext => match self.ui.active_panel {
                ActivePanel::Mcp => {
                    if !self.persistent.mcp_servers.is_empty() {
                        self.ui.selected_mcp =
                            (self.ui.selected_mcp + 1) % self.persistent.mcp_servers.len();
                    }
                }
                ActivePanel::Skills => {
                    if !self.persistent.skills.is_empty() {
                        self.ui.selected_skill =
                            (self.ui.selected_skill + 1) % self.persistent.skills.len();
                    }
                }
                ActivePanel::Behavior => {
                    let total = self.behavior_rows().len();
                    if total > 0 {
                        self.ui.selected_behavior = (self.ui.selected_behavior + 1) % total;
                    }
                }
                ActivePanel::Session => {}
            },
            Action::SelectPrevious => match self.ui.active_panel {
                ActivePanel::Mcp => {
                    if !self.persistent.mcp_servers.is_empty() {
                        self.ui.selected_mcp = if self.ui.selected_mcp == 0 {
                            self.persistent.mcp_servers.len() - 1
                        } else {
                            self.ui.selected_mcp - 1
                        };
                    }
                }
                ActivePanel::Skills => {
                    if !self.persistent.skills.is_empty() {
                        self.ui.selected_skill = if self.ui.selected_skill == 0 {
                            self.persistent.skills.len() - 1
                        } else {
                            self.ui.selected_skill - 1
                        };
                    }
                }
                ActivePanel::Behavior => {
                    let total = self.behavior_rows().len();
                    if total > 0 {
                        self.ui.selected_behavior = if self.ui.selected_behavior == 0 {
                            total - 1
                        } else {
                            self.ui.selected_behavior - 1
                        };
                    }
                }
                ActivePanel::Session => {}
            },
            Action::Quit => self.ui.should_quit = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use hermes_core::config::AppConfig;

    use super::*;

    #[test]
    fn reducer_updates_mcp_skills_and_behavior_selection() {
        let mut state = AppState::new(AppConfig::default(), String::new(), false);
        state.reduce(Action::SyncMcp(vec![McpServerItem {
            name: "demo".to_string(),
            transport: hermes_core::config::McpTransportKind::Http,
            endpoint: "http://localhost".to_string(),
            enabled: true,
            connected: true,
            tool_count: 2,
        }]));
        state.reduce(Action::SetActivePanel(ActivePanel::Behavior));
        state.reduce(Action::SelectNext);

        assert_eq!(state.persistent.mcp_servers.len(), 1);
        assert_eq!(state.ui.selected_behavior, 1);

        state.reduce(Action::SyncSkills(vec![SkillItem {
            name: "build".to_string(),
            description: "Build things".to_string(),
            version: "0.1.0".to_string(),
            available: true,
        }]));
        state.reduce(Action::SetActivePanel(ActivePanel::Skills));
        state.reduce(Action::SelectNext);

        assert_eq!(state.persistent.skills.len(), 1);
        assert_eq!(state.ui.selected_skill, 0);
    }
}
