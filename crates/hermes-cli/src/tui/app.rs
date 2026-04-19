use std::collections::HashMap;
use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use hermes_core::agent::{AgentEvent, HermesAgent};
use hermes_core::client::Message;
use hermes_core::config::{AppConfig, McpServerConfig, McpTransportKind};
use hermes_core::mcp::McpManager;
use hermes_core::skills::SkillManager;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::create_runtime_agent;
use crate::tui::action::Action;
use crate::tui::forms::{Modal, SubmittedMcpForm};
use crate::tui::render;
use crate::tui::state::{ActivePanel, AppState, InputMode, McpServerItem, SkillItem, ViewMode};

pub enum LaunchMode {
    Landing,
    Query(String),
}

pub struct TuiApp {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: AppState,
    system_prompt: Option<String>,
    agent: Option<Arc<HermesAgent>>,
    event_tx: mpsc::Sender<AgentEvent>,
    event_rx: mpsc::Receiver<AgentEvent>,
    run_handle: Option<JoinHandle<hermes_core::Result<Message>>>,
    mcp_manager: McpManager,
    skill_manager: SkillManager,
}

impl TuiApp {
    pub async fn enter(
        config: AppConfig,
        system_prompt: Option<String>,
        launch: LaunchMode,
    ) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        let (event_tx, event_rx) = mpsc::channel(config.tools.event_channel_size);
        let prompt = match &launch {
            LaunchMode::Landing => String::new(),
            LaunchMode::Query(query) => query.clone(),
        };
        let mut app = Self {
            terminal,
            state: AppState::new(
                config.clone(),
                prompt,
                matches!(launch, LaunchMode::Query(_)),
            ),
            system_prompt,
            agent: None,
            event_tx,
            event_rx,
            run_handle: None,
            mcp_manager: McpManager::new(),
            skill_manager: SkillManager::new(config.skills.root_dir.clone()),
        };
        app.refresh_skills()?;
        app.refresh_mcp().await?;
        if matches!(launch, LaunchMode::Query(_)) {
            app.start_run().await?;
        }
        Ok(app)
    }

    pub async fn run(mut self) -> Result<()> {
        loop {
            let size = self.terminal.size()?;
            self.state.set_layout_for_width(size.width);
            self.drain_events();
            self.finish_run_if_ready().await?;

            self.terminal
                .draw(|frame| render::render(frame, &self.state))?;

            if self.state.ui.should_quit {
                break;
            }

            if event::poll(Duration::from_millis(
                self.state.persistent.config.tui.refresh_rate_ms,
            ))? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key).await?;
                }
            }
        }

        self.exit()
    }

    fn exit(mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            self.state.reduce(Action::AgentEvent(event));
        }
    }

    async fn finish_run_if_ready(&mut self) -> Result<()> {
        if let Some(handle) = &self.run_handle {
            if handle.is_finished() {
                let handle = self.run_handle.take().unwrap();
                match handle.await.context("agent task join failed")? {
                    Ok(_) => {
                        self.state.reduce(Action::SetFooter(
                            "Run complete. Press i for next prompt or tab for management panels."
                                .to_string(),
                        ));
                    }
                    Err(error) => {
                        self.state.reduce(Action::RunFailed(error.to_string()));
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.state.ui.modal.is_some() {
            return self.handle_modal_key(key).await;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
            if let Some(agent) = &self.agent {
                agent.clear_history().await;
            }
            self.state.reduce(Action::ClearSession);
            if self.state.persistent.needs_rebuild {
                self.agent = None;
            }
            self.state.reduce(Action::SetFooter(
                "New session ready. Pending behavior and MCP changes will apply now.".to_string(),
            ));
            return Ok(());
        }

        match self.state.ui.input_mode {
            InputMode::Prompt => self.handle_prompt_key(key).await,
            InputMode::Command => self.handle_command_key(key).await,
        }
    }

    async fn handle_prompt_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.state.reduce(Action::SetInputMode(InputMode::Command)),
            KeyCode::Enter => self.start_run().await?,
            KeyCode::Backspace => self.state.reduce(Action::PromptBackspace),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.reduce(Action::AppendPrompt(ch));
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_command_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.state.reduce(Action::Quit),
            KeyCode::Char('i') => self.state.reduce(Action::SetInputMode(InputMode::Prompt)),
            KeyCode::Tab => self.state.reduce(Action::CyclePanelForward),
            KeyCode::BackTab => self.state.reduce(Action::CyclePanelBackward),
            KeyCode::Char('w') => self
                .state
                .reduce(Action::SetActivePanel(ActivePanel::Session)),
            KeyCode::Char('m') => self.state.reduce(Action::SetActivePanel(ActivePanel::Mcp)),
            KeyCode::Char('s') => self
                .state
                .reduce(Action::SetActivePanel(ActivePanel::Skills)),
            KeyCode::Char('b') => self
                .state
                .reduce(Action::SetActivePanel(ActivePanel::Behavior)),
            KeyCode::Up => self.state.reduce(Action::SelectPrevious),
            KeyCode::Down => self.state.reduce(Action::SelectNext),
            KeyCode::Char('a') if self.state.ui.active_panel == ActivePanel::Mcp => {
                self.state.reduce(Action::OpenModal(Modal::add_mcp()));
            }
            KeyCode::Char('n') if self.state.ui.active_panel == ActivePanel::Skills => {
                self.state.reduce(Action::OpenModal(Modal::create_skill()));
            }
            KeyCode::Char('r') if self.state.ui.active_panel == ActivePanel::Skills => {
                self.refresh_skills()?;
                self.state
                    .reduce(Action::SetFooter("Skills reloaded from disk.".to_string()));
            }
            KeyCode::Char('d') if self.state.ui.active_panel == ActivePanel::Mcp => {
                self.remove_selected_mcp().await?;
            }
            KeyCode::Char('d') if self.state.ui.active_panel == ActivePanel::Skills => {
                self.remove_selected_skill()?;
            }
            KeyCode::Char('e') if self.state.ui.active_panel == ActivePanel::Behavior => {
                self.open_behavior_editor();
            }
            KeyCode::Char(' ') if self.state.ui.active_panel == ActivePanel::Behavior => {
                self.toggle_selected_behavior();
            }
            KeyCode::Esc => self.state.reduce(Action::SetView(ViewMode::Landing)),
            _ => {}
        }
        Ok(())
    }

    async fn handle_modal_key(&mut self, key: KeyEvent) -> Result<()> {
        let mut modal = self.state.ui.modal.clone().unwrap();
        match key.code {
            KeyCode::Esc => {
                self.state.reduce(Action::CloseModal);
                return Ok(());
            }
            KeyCode::Tab => modal.next_field(),
            KeyCode::BackTab => modal.previous_field(),
            KeyCode::Backspace => modal.backspace(),
            KeyCode::Enter => {
                self.submit_modal(modal).await?;
                self.state.reduce(Action::CloseModal);
                return Ok(());
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                modal.push_char(ch)
            }
            _ => {}
        }
        self.state.reduce(Action::UpdateModal(modal));
        Ok(())
    }

    async fn submit_modal(&mut self, modal: Modal) -> Result<()> {
        match modal {
            Modal::AddMcp(form) => {
                let submitted = parse_mcp_form(&form)?;
                let config = McpServerConfig {
                    name: submitted.name.clone(),
                    transport: submitted.transport.clone(),
                    url: submitted.url.clone(),
                    auth_token: submitted.auth_token.clone(),
                    command: submitted.command.clone(),
                    args: submitted.args.clone(),
                    env: submitted.env.clone(),
                    enabled: true,
                };
                self.upsert_mcp_server(config, submitted).await?;
            }
            Modal::CreateSkill(form) => {
                let name = form.fields[0].value.trim();
                let description = form.fields[1].value.trim();
                if name.is_empty() {
                    self.state
                        .reduce(Action::SetFooter("Skill name cannot be empty.".to_string()));
                } else {
                    let content = default_skill_content(name, description);
                    self.skill_manager.create(name, &content)?;
                    self.refresh_skills()?;
                    self.state
                        .reduce(Action::SetFooter(format!("Created skill '{}'.", name)));
                }
            }
            Modal::EditBehavior(form) => {
                let field = form.fields[0].value.trim().to_string();
                let value = form.fields[1].value.trim().to_string();
                self.apply_behavior_edit(&field, &value)?;
            }
        }
        Ok(())
    }

    async fn start_run(&mut self) -> Result<()> {
        if self.state.session.running {
            return Ok(());
        }

        let query = self.state.ui.prompt_input.trim().to_string();
        if query.is_empty() {
            self.state.reduce(Action::SetFooter(
                "Prompt is empty. Press i and type something first.".to_string(),
            ));
            return Ok(());
        }

        if self.state.persistent.needs_rebuild && !self.state.session.transcript.is_empty() {
            let prompt = query.clone();
            self.state.reduce(Action::ClearSession);
            self.state.ui.view = ViewMode::Workspace;
            self.state.ui.prompt_input = prompt;
        }

        if self.agent.is_none() || self.state.persistent.needs_rebuild {
            self.rebuild_agent().await?;
        }

        let agent = self.agent.clone().context("agent was not available")?;
        self.state.reduce(Action::StartRun(query.clone()));
        self.state.reduce(Action::ClearPrompt);
        self.run_handle = Some(tokio::spawn(async move { agent.run(query).await }));
        Ok(())
    }

    async fn rebuild_agent(&mut self) -> Result<()> {
        let (event_tx, event_rx) =
            mpsc::channel(self.state.persistent.config.tools.event_channel_size);
        self.event_tx = event_tx.clone();
        self.event_rx = event_rx;
        let agent = create_runtime_agent(
            &self.state.persistent.config,
            &self.state.persistent.behavior,
            self.system_prompt.as_deref(),
            event_tx,
            &mut self.mcp_manager,
        )
        .await?;
        self.agent = Some(Arc::new(agent));
        self.state.persistent.needs_rebuild = false;
        Ok(())
    }

    async fn refresh_mcp(&mut self) -> Result<()> {
        let mut items = Vec::new();
        for server in &self.state.persistent.config.mcp.servers {
            let connected = match self.mcp_manager.get(&server.name) {
                Some(transport) => transport.is_connected().await,
                None => false,
            };
            let tool_count = match self.mcp_manager.get(&server.name) {
                Some(transport) if connected => transport.get_tools().await.len(),
                _ => 0,
            };
            items.push(McpServerItem {
                name: server.name.clone(),
                transport: server.transport.clone(),
                endpoint: server
                    .url
                    .clone()
                    .or_else(|| server.command.clone())
                    .unwrap_or_default(),
                enabled: server.enabled,
                connected,
                tool_count,
            });
        }
        self.state.reduce(Action::SyncMcp(items));
        Ok(())
    }

    fn refresh_skills(&mut self) -> Result<()> {
        std::fs::create_dir_all(&self.state.persistent.config.skills.root_dir)?;
        let loaded = self.skill_manager.load_all()?;
        let items = loaded
            .iter()
            .map(|skill| SkillItem {
                name: skill.name.clone(),
                description: skill.description.clone(),
                version: skill.version.clone(),
                available: self.skill_manager.is_available(skill),
            })
            .collect::<Vec<_>>();
        self.state.reduce(Action::SyncSkills(items));
        Ok(())
    }

    async fn upsert_mcp_server(
        &mut self,
        config: McpServerConfig,
        submitted: SubmittedMcpForm,
    ) -> Result<()> {
        match submitted.transport {
            McpTransportKind::Http => {
                let url = submitted
                    .url
                    .clone()
                    .context("MCP HTTP server requires a URL.")?;
                self.mcp_manager
                    .add_server(config.name.clone(), url, submitted.auth_token.clone())
                    .await?;
            }
            McpTransportKind::Stdio => {
                let command = submitted
                    .command
                    .clone()
                    .context("MCP stdio server requires a command.")?;
                self.mcp_manager
                    .add_stdio_server(
                        config.name.clone(),
                        command,
                        submitted.args.clone(),
                        submitted.env.clone(),
                    )
                    .await?;
            }
        }

        if let Some(existing) = self
            .state
            .persistent
            .config
            .mcp
            .servers
            .iter_mut()
            .find(|server| server.name == config.name)
        {
            *existing = config.clone();
        } else {
            self.state.persistent.config.mcp.servers.push(config);
        }
        self.state.persistent.needs_rebuild = true;
        self.refresh_mcp().await?;
        self.state.reduce(Action::SetFooter(
            "MCP server saved. Press ctrl+l for a fresh session with updated tools.".to_string(),
        ));
        Ok(())
    }

    async fn remove_selected_mcp(&mut self) -> Result<()> {
        if let Some(server) = self
            .state
            .persistent
            .mcp_servers
            .get(self.state.ui.selected_mcp)
            .cloned()
        {
            self.mcp_manager.remove_server(&server.name).await?;
            self.state
                .persistent
                .config
                .mcp
                .servers
                .retain(|item| item.name != server.name);
            self.state.persistent.needs_rebuild = true;
            self.refresh_mcp().await?;
            self.state.reduce(Action::SetFooter(format!(
                "Removed MCP server '{}'.",
                server.name
            )));
        }
        Ok(())
    }

    fn remove_selected_skill(&mut self) -> Result<()> {
        if let Some(skill) = self
            .state
            .persistent
            .skills
            .get(self.state.ui.selected_skill)
            .cloned()
        {
            self.skill_manager.delete(&skill.name)?;
            self.refresh_skills()?;
            self.state.reduce(Action::SetFooter(format!(
                "Deleted skill '{}'.",
                skill.name
            )));
        }
        Ok(())
    }

    fn open_behavior_editor(&mut self) {
        let rows = self.state.behavior_rows();
        if let Some((field, value)) = rows.get(self.state.ui.selected_behavior) {
            self.state
                .reduce(Action::OpenModal(Modal::edit_behavior(field, value)));
        }
    }

    fn toggle_selected_behavior(&mut self) {
        let rows = self.state.behavior_rows();
        if let Some((field, value)) = rows.get(self.state.ui.selected_behavior) {
            if value == "true" || value == "false" {
                let next = if value == "true" { "false" } else { "true" };
                if self.apply_behavior_edit(field, next).is_ok() {
                    self.state.reduce(Action::SetFooter(format!(
                        "Updated {}. Press ctrl+l to apply to a fresh session.",
                        field
                    )));
                }
            }
        }
    }

    fn apply_behavior_edit(&mut self, field: &str, value: &str) -> Result<()> {
        let behavior = &mut self.state.persistent.behavior;
        match field {
            "model" => behavior.model = value.to_string(),
            "system_prompt" => {
                behavior.system_prompt = if value.is_empty() || value == "(default)" {
                    None
                } else {
                    Some(value.to_string())
                }
            }
            "max_iterations" => behavior.max_iterations = value.parse()?,
            "tool_timeout_secs" => behavior.tool_timeout_secs = value.parse()?,
            "request_timeout_secs" => behavior.request_timeout_secs = value.parse()?,
            "context_window" => behavior.context_window = value.parse()?,
            "stream" => behavior.stream = parse_bool(value)?,
            "show_reasoning" => behavior.show_reasoning = parse_bool(value)?,
            "max_healing_attempts" => behavior.max_healing_attempts = value.parse()?,
            _ => {}
        }

        self.state.persistent.config.agent = behavior.clone();
        self.state.session.max_iterations = behavior.max_iterations;
        self.state.persistent.needs_rebuild = true;
        self.state.reduce(Action::SetFooter(format!(
            "Updated {}. Press ctrl+l to start a fresh session with new behavior.",
            field
        )));
        Ok(())
    }
}

impl Drop for TuiApp {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn parse_mcp_form(form: &crate::tui::forms::FormState) -> Result<SubmittedMcpForm> {
    let transport = match form.fields[0].value.trim().to_ascii_lowercase().as_str() {
        "http" => McpTransportKind::Http,
        "stdio" => McpTransportKind::Stdio,
        other => anyhow::bail!("Unknown MCP transport '{}'. Use http or stdio.", other),
    };
    let name = form.fields[1].value.trim().to_string();
    if name.is_empty() {
        anyhow::bail!("MCP server name cannot be empty.");
    }

    Ok(SubmittedMcpForm {
        transport,
        name,
        url: non_empty(form.fields[2].value.trim()),
        auth_token: non_empty(form.fields[3].value.trim()),
        command: non_empty(form.fields[4].value.trim()),
        args: split_csv(&form.fields[5].value),
        env: parse_env_pairs(&form.fields[6].value)?,
    })
}

fn default_skill_content(name: &str, description: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {description}\nversion: 0.1.0\n---\n# {name}\n\nDescribe the workflow here.\n"
    )
}

fn parse_env_pairs(value: &str) -> Result<HashMap<String, String>> {
    let mut pairs = HashMap::new();
    for item in split_csv(value) {
        let Some((key, value)) = item.split_once('=') else {
            anyhow::bail!("Invalid env entry '{}'. Use KEY=VALUE.", item);
        };
        pairs.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(pairs)
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("Expected a boolean value."),
    }
}
