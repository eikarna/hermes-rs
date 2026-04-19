//! Hermes-RS CLI
//!
//! A command-line interface for the Hermes-RS agent framework.

mod tui;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use console::{style, Emoji};
use hermes_core::agent::{AgentConfig, AgentEvent, HermesAgent};
use hermes_core::client::{ClientConfig, OpenAIClient};
use hermes_core::tools::{HermesTool, ToolContext, ToolRegistry};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::tui::LiveRunTui;

static DONE: Emoji<'_, '_> = Emoji("✅", "");
static ERROR: Emoji<'_, '_> = Emoji("❌", "");

/// Hermes-RS CLI arguments
#[derive(Debug, Parser)]
#[command(
    name = "hermes",
    about = "Hermes-RS: A high-performance ReAct agent framework",
    long_about = None,
    version
)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Log level (trace, debug, info, warn, error, off)
    #[arg(short, long, global = true)]
    log_level: Option<String>,

    /// Configuration file path
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// OpenAI API key
    #[arg(long, global = true, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    /// OpenAI base URL
    #[arg(
        long,
        global = true,
        env = "OPENAI_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    base_url: String,

    /// Model to use
    #[arg(long, global = true, default_value = "gpt-4")]
    model: String,

    /// Maximum iterations
    #[arg(long, global = true, default_value = "20")]
    max_iterations: usize,

    /// Tool timeout in seconds
    #[arg(long, global = true, default_value = "30")]
    tool_timeout: u64,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the agent interactively
    Run {
        /// System prompt
        #[arg(short, long)]
        system: Option<String>,

        /// Initial query (if not interactive)
        #[arg(short, long)]
        query: Option<String>,
    },

    /// List available tools
    Tools {
        /// Show full schema for each tool
        #[arg(short, long)]
        verbose: bool,
    },

    /// Chat mode (interactive conversation)
    Chat {
        /// System prompt
        #[arg(short, long)]
        system: Option<String>,
    },

    /// Test a specific tool
    Test {
        /// Tool name
        #[arg()]
        tool_name: String,

        /// Tool arguments as JSON
        #[arg(short, long)]
        args: Option<String>,
    },
}

/// Configuration from file
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    model: Option<String>,
    max_iterations: Option<usize>,
    tool_timeout: Option<u64>,
    system_prompt: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    request_timeout: Option<u64>,
    context_window: Option<usize>,
    stream: Option<bool>,
    max_healing_attempts: Option<usize>,
    tool_registry_timeout: Option<u64>,
    event_channel_size: Option<usize>,
    #[serde(default)]
    logging: LoggingConfig,
    #[serde(default)]
    ui: UiConfig,
}

#[derive(Debug, Deserialize)]
struct LoggingConfig {
    level: Option<String>,
    format: Option<String>,
    log_file: Option<String>,
    with_target: Option<bool>,
    with_thread_ids: Option<bool>,
    with_file: Option<bool>,
    with_line_number: Option<bool>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: Some("info".to_string()),
            format: Some("pretty".to_string()),
            log_file: None,
            with_target: Some(false),
            with_thread_ids: Some(false),
            with_file: Some(false),
            with_line_number: Some(false),
        }
    }
}

#[derive(Debug, Deserialize)]
struct UiConfig {
    #[allow(dead_code)]
    theme: Option<String>,
    show_thinking: Option<bool>,
    show_tool_calls: Option<bool>,
    show_iterations: Option<bool>,
    rich_output: Option<bool>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: Some("dark".to_string()),
            show_thinking: Some(true),
            show_tool_calls: Some(true),
            show_iterations: Some(true),
            rich_output: Some(true),
        }
    }
}

impl FileConfig {
    fn merge_with(&self, cli: &Cli) -> ClientConfig {
        let timeout_secs = self.request_timeout.unwrap_or(120);
        ClientConfig {
            base_url: self
                .base_url
                .clone()
                .unwrap_or_else(|| cli.base_url.clone()),
            api_key: self.api_key.clone().or(cli.api_key.clone()),
            timeout: Duration::from_secs(timeout_secs),
            max_context_length: self.context_window.unwrap_or(128_000),
        }
    }

    fn agent_config(&self, cli: &Cli, system_prompt: Option<&str>) -> AgentConfig {
        let tool_timeout_secs = self.tool_timeout.unwrap_or(cli.tool_timeout);
        AgentConfig {
            model: self.model.clone().unwrap_or_else(|| cli.model.clone()),
            max_iterations: self.max_iterations.unwrap_or(cli.max_iterations),
            tool_timeout: Duration::from_secs(tool_timeout_secs),
            request_timeout: Duration::from_secs(self.request_timeout.unwrap_or(120)),
            system_prompt: self
                .system_prompt
                .clone()
                .or_else(|| system_prompt.map(String::from)),
            stream: self.stream.unwrap_or(true),
            context_window: self.context_window.unwrap_or(128_000),
            max_healing_attempts: self.max_healing_attempts.unwrap_or(3),
        }
    }

    fn tool_registry_timeout(&self) -> Duration {
        Duration::from_secs(self.tool_registry_timeout.unwrap_or(30))
    }

    fn event_channel_size(&self) -> usize {
        self.event_channel_size.unwrap_or(100)
    }
}

fn is_log_level_off(s: &str) -> bool {
    let s = s.to_lowercase();
    s == "off" || s == "none"
}

fn build_file_env_filter(file_config: &LoggingConfig) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if let Some(ref level) = file_config.level {
            if is_log_level_off(level) {
                EnvFilter::new("off")
            } else {
                EnvFilter::new(level)
            }
        } else {
            EnvFilter::new(format!("{}", Level::INFO))
        }
    })
}

/// Initialize logging — all logs go to stderr, completely separate from chat stdout
fn init_logging(verbose: bool, cli_log_level: Option<&str>, file_config: &LoggingConfig) {
    let env_filter = if verbose {
        EnvFilter::new(format!("{}", Level::DEBUG))
    } else if let Some(cli_lvl) = cli_log_level {
        if !cli_lvl.is_empty() && is_log_level_off(cli_lvl) {
            EnvFilter::new("off")
        } else if !cli_lvl.is_empty() {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(cli_lvl))
        } else {
            build_file_env_filter(file_config)
        }
    } else {
        build_file_env_filter(file_config)
    };

    let format = file_config.format.as_deref().unwrap_or("pretty");

    let subscriber = tracing_subscriber::registry().with(env_filter);

    if let Some(ref log_file) = file_config.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .expect("Failed to open log file");
        let file_writer = std::sync::Mutex::new(file);
        let file_layer = fmt::layer()
            .with_writer(file_writer)
            .with_ansi(false)
            .json();
        match format {
            "compact" => subscriber.with(file_layer.compact()).init(),
            _ => subscriber.with(file_layer).init(),
        }
    } else {
        let layer = fmt::layer()
            .with_target(file_config.with_target.unwrap_or(false))
            .with_thread_ids(file_config.with_thread_ids.unwrap_or(false))
            .with_file(file_config.with_file.unwrap_or(false))
            .with_line_number(file_config.with_line_number.unwrap_or(false));
        match format {
            "json" => subscriber.with(layer.json()).init(),
            "compact" => subscriber.with(layer.compact()).init(),
            _ => subscriber.with(layer.pretty()).init(),
        }
    }
}

/// Print the Hermes banner
fn print_banner() {
    let banner = r#"
╔═══════════════════════════════════════════════════════════════════════════╗
║                                                                           ║
║ ██╗  ██╗███████╗██████╗ ███╗   ███╗███████╗███████╗      ██████╗ ███████╗ ║
║ ██║  ██║██╔════╝██╔══██╗████╗ ████║██╔════╝██╔════╝      ██╔══██╗██╔════╝ ║
║ ███████║█████╗  ██████╔╝██╔████╔██║█████╗  ███████╗█████╗██████╔╝███████╗ ║
║ ██╔══██║██╔══╝  ██╔══██╗██║╚██╔╝██║██╔══╝  ╚════██║╚════╝██╔══██╗╚════██║ ║
║ ██║  ██║███████╗██║  ██║██║ ╚═╝ ██║███████╗███████║      ██║  ██║███████║ ║
║ ╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝╚══════╝╚══════╝      ╚═╝  ╚═╝╚══════╝ ║
║                                                                           ║
╚═══════════════════════════════════════════════════════════════════════════╝
                    High-Performance AI Agent Framework
"#;
    println!("{}", style(banner).cyan());
    println!();
}

fn print_assistant_message(text: &str) {
    println!("{}", style("Assistant:").cyan().bold());
    let lines: Vec<&str> = text.lines().collect();
    for line in lines {
        println!("  {}", line);
    }
    println!();
}

fn print_done(message: &str) {
    println!("\n{}", style("═".repeat(60)).dim());
    println!("{}", style("Final Response:").cyan().bold());
    let lines: Vec<&str> = message.lines().collect();
    for line in lines {
        println!("  {}", line);
    }
    println!("{}\n", style("═".repeat(60)).dim());
}

fn print_error(text: &str) {
    eprintln!(
        "{} {}",
        style(format!("{}", ERROR)).red(),
        style(text).red()
    );
}

async fn run_with_tui(
    agent: &HermesAgent,
    event_rx: &mut mpsc::Receiver<AgentEvent>,
    query: &str,
    model: &str,
    max_iterations: usize,
    ui: &UiConfig,
) -> Result<hermes_core::client::Message> {
    while event_rx.try_recv().is_ok() {}

    let mut tui = LiveRunTui::enter(
        model,
        query,
        max_iterations,
        ui.show_thinking.unwrap_or(true),
        ui.show_tool_calls.unwrap_or(true),
        ui.show_iterations.unwrap_or(true),
    )?;

    let run_future = agent.run(query.to_string());
    tokio::pin!(run_future);
    let mut ticker = tokio::time::interval(Duration::from_millis(80));
    let mut result: Option<
        std::result::Result<hermes_core::client::Message, hermes_core::error::Error>,
    > = None;

    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                if let Some(event) = maybe_event {
                    tui.apply_event(&event);
                }
            }
            response = &mut run_future, if result.is_none() => {
                result = Some(response);
            }
            _ = ticker.tick() => {}
        }

        tui.draw()?;

        if result.is_some() && event_rx.is_empty() {
            break;
        }
    }

    tui.exit()?;

    result
        .context("agent run did not produce a result")?
        .map_err(anyhow::Error::from)
}

/// Build the tool registry with built-in tools
async fn build_registry(timeout: Duration) -> ToolRegistry {
    let registry = ToolRegistry::new(timeout);
    hermes_core::tools::register_builtin_tools(&registry)
        .await
        .unwrap();
    registry.register(EchoTool::new()).await.unwrap();
    registry.register(CalculatorTool::new()).await.unwrap();
    registry
}

/// Simple echo tool for testing
struct EchoTool;

impl EchoTool {
    fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl HermesTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo back the input message. Useful for testing."
    }

    fn schema(&self) -> hermes_core::schema::ToolSchema {
        use schemars::JsonSchema;

        #[derive(JsonSchema, Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct EchoArgs {
            message: String,
        }

        hermes_core::schema::ToolSchema::from_type::<EchoArgs>(
            "echo",
            "Echo back the input message",
        )
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> hermes_core::tools::ToolResult {
        if let Some(msg) = args.get("message").and_then(|v| v.as_str()) {
            hermes_core::tools::ToolResult::success("echo", serde_json::json!({ "echoed": msg }))
        } else {
            hermes_core::tools::ToolResult::error("echo", "Missing 'message' argument")
        }
    }
}

/// Calculator tool
struct CalculatorTool;

impl CalculatorTool {
    fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl HermesTool for CalculatorTool {
    fn name(&self) -> &str {
        "calculate"
    }

    fn description(&self) -> &str {
        "Perform a calculation. Supports basic operations: add, subtract, multiply, divide."
    }

    fn schema(&self) -> hermes_core::schema::ToolSchema {
        use schemars::JsonSchema;

        #[derive(JsonSchema, Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct CalcArgs {
            operation: String,
            a: f64,
            b: f64,
        }

        hermes_core::schema::ToolSchema::from_type::<CalcArgs>("calculate", "Perform calculations")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> hermes_core::tools::ToolResult {
        let op = args
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("add");
        let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let result = match op {
            "add" | "addition" | "+" => a + b,
            "subtract" | "subtraction" | "-" => a - b,
            "multiply" | "multiplication" | "*" | "x" => a * b,
            "divide" | "division" | "/" => {
                if b == 0.0 {
                    return hermes_core::tools::ToolResult::error("calculate", "Division by zero");
                }
                a / b
            }
            _ => {
                return hermes_core::tools::ToolResult::error(
                    "calculate",
                    format!("Unknown operation: {}", op),
                );
            }
        };

        hermes_core::tools::ToolResult::success(
            "calculate",
            serde_json::json!({
                "operation": op,
                "operand_a": a,
                "operand_b": b,
                "result": result
            }),
        )
    }
}

/// Run the agent with a single query, streaming events
async fn run_agent(
    cli: &Cli,
    config: &FileConfig,
    system_prompt: Option<&str>,
    query: &str,
) -> Result<()> {
    let client_config = config.merge_with(cli);
    let client = OpenAIClient::new(client_config);
    let registry = build_registry(config.tool_registry_timeout()).await;
    let agent_config = config.agent_config(cli, system_prompt);
    let max_iterations = agent_config.max_iterations;

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(config.event_channel_size());
    let agent = HermesAgent::with_events(agent_config.clone(), client, registry, event_tx);

    let ui = &config.ui;
    let rich = ui.rich_output.unwrap_or(true);
    let show_thinking = ui.show_thinking.unwrap_or(true);
    let show_tool_calls = ui.show_tool_calls.unwrap_or(true);
    let show_iterations = ui.show_iterations.unwrap_or(true);

    if rich {
        let response = run_with_tui(
            &agent,
            &mut event_rx,
            query,
            &agent_config.model,
            max_iterations,
            ui,
        )
        .await?;
        print_done(&response.content);
    } else {
        let _agent_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    AgentEvent::Thinking { content } => {
                        if show_thinking {
                            println!("[Thinking] {}", content);
                        }
                    }
                    AgentEvent::Reasoning { text } => {
                        if show_thinking {
                            println!("[Reasoning] {}", text);
                        }
                    }
                    AgentEvent::ToolStart { name, arguments } => {
                        if show_tool_calls {
                            println!("\n[Tool] Calling: {} with {}", name, arguments);
                        }
                    }
                    AgentEvent::ToolComplete { result } => {
                        if show_tool_calls {
                            if result.success {
                                println!("[Tool] Result: {}\n", result.content);
                            } else {
                                println!(
                                    "[Tool] Error: {}\n",
                                    result.error.clone().unwrap_or_default()
                                );
                            }
                        }
                    }
                    AgentEvent::ToolError { name, error } => {
                        if show_tool_calls {
                            println!("[Tool] {} failed: {}\n", name, error);
                        }
                    }
                    AgentEvent::Content { text } => {
                        print!("{}", text);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                    }
                    AgentEvent::Done { message } => {
                        println!("\n\n[Done] Final response: {}\n", message.content);
                    }
                    AgentEvent::IterationComplete { iteration } => {
                        if show_iterations {
                            println!("\n[Iteration {} complete]", iteration);
                        }
                    }
                    AgentEvent::Error { error } => {
                        eprintln!("\n[Error] {}\n", error);
                    }
                }
            }
        });

        match agent.run(query.to_string()).await {
            Ok(response) => {
                println!("\nAgent Response:\n{}", response.content);
            }
            Err(e) => {
                error!("Agent failed: {}", e);
                anyhow::bail!("Agent error: {}", e);
            }
        }
    }

    Ok(())
}

/// List available tools
async fn list_tools(config: &FileConfig, verbose: bool) -> Result<()> {
    let registry = build_registry(config.tool_registry_timeout()).await;
    let tools = registry.get_schemas().await;

    if tools.is_empty() {
        println!("No tools registered.");
        return Ok(());
    }

    println!(
        "\n{} Available tools ({} total):\n",
        style("🔧").yellow(),
        tools.len()
    );

    for (i, tool) in tools.iter().enumerate() {
        println!(
            "  {}. {}: {}",
            style(i + 1).cyan().bold(),
            style(&tool.name).white().bold(),
            style(&tool.description).dim()
        );

        if verbose {
            println!(
                "     Schema: {}",
                serde_json::to_string_pretty(&tool.parameters).unwrap_or_default()
            );
        }
        println!();
    }

    Ok(())
}

/// Interactive chat mode
async fn chat_mode(cli: &Cli, config: &FileConfig, system_prompt: Option<&str>) -> Result<()> {
    use std::io::{self, Write};

    let rich = config.ui.rich_output.unwrap_or(true);

    if rich {
        print_banner();
        println!(
            "  {} Type 'exit' or 'quit' to end the conversation.",
            style("💡").dim()
        );
        println!(
            "  {} Type 'clear' to clear conversation history.\n",
            style("💡").dim()
        );
        println!("{}", style("─".repeat(60)).dim());
    } else {
        println!("Hermes-RS Chat Mode");
        println!("Type 'exit' or 'quit' to end the conversation.\n");
    }

    let client_config = config.merge_with(cli);
    let client = OpenAIClient::new(client_config);
    let registry = build_registry(config.tool_registry_timeout()).await;
    let agent_config = config.agent_config(cli, system_prompt);

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(config.event_channel_size());
    let agent = HermesAgent::with_events(agent_config.clone(), client, registry, event_tx);

    loop {
        if rich {
            print!("{} ", style("You:").green().bold());
        } else {
            print!("You: ");
        }
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
            if rich {
                println!("\n{}", style("Goodbye! 👋").cyan());
            } else {
                println!("Goodbye!");
            }
            break;
        }

        if input.eq_ignore_ascii_case("clear") {
            agent.clear_history().await;
            if rich {
                println!("\n{} Conversation history cleared.\n", style(DONE).green());
            } else {
                println!("Conversation history cleared.\n");
            }
            continue;
        }

        match if rich {
            run_with_tui(
                &agent,
                &mut event_rx,
                input,
                &agent_config.model,
                agent_config.max_iterations,
                &config.ui,
            )
            .await
        } else {
            agent
                .run(input.to_string())
                .await
                .map_err(anyhow::Error::from)
        } {
            Ok(response) => {
                if rich {
                    println!("{}", style("─".repeat(60)).dim());
                    print_assistant_message(&response.content);
                } else {
                    println!("\nAssistant: {}\n", response.content);
                }
            }
            Err(e) => {
                if rich {
                    print_error(&format!("Agent error: {}", e));
                } else {
                    eprintln!("Error: {}\n", e);
                }
            }
        }
    }

    Ok(())
}

/// Test a specific tool
async fn test_tool(
    _cli: &Cli,
    config: &FileConfig,
    tool_name: &str,
    args: Option<&str>,
) -> Result<()> {
    let registry = build_registry(config.tool_registry_timeout()).await;

    if !registry.contains(tool_name).await {
        anyhow::bail!("Tool '{}' not found", tool_name);
    }

    println!("\nTesting tool: {}", style(tool_name).cyan().bold());
    println!(
        "Arguments: {}",
        serde_json::to_string_pretty(&args.map(String::from).unwrap_or_default())
            .unwrap_or_default()
    );

    let parsed_args: Value = if let Some(args_str) = args {
        serde_json::from_str(args_str).context("Failed to parse tool arguments as JSON")?
    } else {
        Value::Object(serde_json::Map::new())
    };

    let result = registry
        .execute(
            tool_name,
            &format!("test_{}", tool_name),
            parsed_args,
            ToolContext::default(),
        )
        .await?;

    println!("\n{} Result:", style("📋").yellow());
    println!("  Success: {}", result.success);
    println!("  Content: {}", result.content);
    if let Some(error) = result.error {
        println!("  Error: {}", style(error).red());
    }

    Ok(())
}

/// Load configuration from file or defaults
fn load_config(cli: &Cli) -> FileConfig {
    if let Some(ref config_path) = cli.config {
        let contents = match std::fs::read_to_string(config_path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Failed to read config file {}: {}",
                    config_path.display(),
                    e
                );
                return FileConfig::default();
            }
        };

        let ext = config_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("yaml");

        match ext {
            "json" => serde_json::from_str(&contents).unwrap_or_else(|e| {
                warn!("Failed to parse JSON config: {}", e);
                FileConfig::default()
            }),
            "toml" => toml::from_str(&contents).unwrap_or_else(|e| {
                warn!("Failed to parse TOML config: {}", e);
                FileConfig::default()
            }),
            _ => serde_yaml::from_str(&contents).unwrap_or_else(|e| {
                warn!("Failed to parse YAML config: {}", e);
                FileConfig::default()
            }),
        }
    } else {
        let default_paths = vec![
            PathBuf::from("hermes.toml"),
            PathBuf::from(".hermes.toml"),
            dirs::config_dir()
                .map(|p| p.join("hermes").join("config.toml"))
                .unwrap_or_default(),
        ];

        for path in default_paths {
            if path.exists() {
                let contents = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let file_config: FileConfig = match ext {
                    "toml" => toml::from_str(&contents).unwrap_or_else(|_| FileConfig::default()),
                    "json" => {
                        serde_json::from_str(&contents).unwrap_or_else(|_| FileConfig::default())
                    }
                    _ => serde_yaml::from_str(&contents).unwrap_or_else(|_| FileConfig::default()),
                };
                info!("Loaded config from: {}", path.display());
                return file_config;
            }
        }

        FileConfig::default()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config(&cli);

    init_logging(cli.verbose, cli.log_level.as_deref(), &config.logging);

    debug!("Hermes-RS CLI starting");
    debug!("Arguments: {:?}", cli);
    debug!("Config: {:?}", config);

    match &cli.command {
        Commands::Run { system, query } => {
            let query = query
                .as_ref()
                .context("No query provided. Use --query or enter interactive mode.")?;
            run_agent(&cli, &config, system.as_deref(), query).await?;
        }
        Commands::Tools { verbose } => {
            list_tools(&config, *verbose).await?;
        }
        Commands::Chat { system } => {
            chat_mode(&cli, &config, system.as_deref()).await?;
        }
        Commands::Test { tool_name, args } => {
            test_tool(&cli, &config, tool_name, args.as_deref()).await?;
        }
    }

    Ok(())
}
