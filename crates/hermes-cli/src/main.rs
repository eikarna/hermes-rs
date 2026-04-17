//! Hermes-RS CLI
//!
//! A command-line interface for the Hermes-RS agent framework.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hermes_core::agent::{AgentConfig, AgentEvent, HermesAgent};
use hermes_core::client::{ClientConfig, OpenAIClient};
use hermes_core::tools::{HermesTool, ToolContext, ToolRegistry};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, error, info, Level};
use tracing_subscriber::FmtSubscriber;

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

    /// Log level
    #[arg(short, long, value_enum, default_value = "info", global = true)]
    log_level: String,

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
#[derive(Debug, Deserialize)]
struct FileConfig {
    model: Option<String>,
    max_iterations: Option<usize>,
    tool_timeout: Option<u64>,
    system_prompt: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
}

impl FileConfig {
    fn merge_with(&self, cli: &Cli) -> ClientConfig {
        let api_key = self.api_key.clone().or(cli.api_key.clone());

        ClientConfig {
            base_url: self
                .base_url
                .clone()
                .unwrap_or_else(|| cli.base_url.clone()),
            api_key,
            timeout: Duration::from_secs(60),
            max_context_length: 128_000,
        }
    }
}

/// Initialize logging
fn init_logging(verbose: bool, level: &str) {
    let level = if verbose {
        Level::DEBUG
    } else {
        level.parse().unwrap_or(Level::INFO)
    };

    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");
}

/// Build the tool registry with built-in tools
fn build_registry() -> ToolRegistry {
    let registry = ToolRegistry::new(Duration::from_secs(30));

    // Register all built-in tools (file, terminal, web, code, memory, http, datetime, todo, clarify, patch)
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            hermes_core::tools::register_builtin_tools(&registry)
                .await
                .unwrap();
            // Also register CLI-specific demo tools
            registry.register(EchoTool::new()).await.unwrap();
            registry.register(CalculatorTool::new()).await.unwrap();
        });

    registry
}

/// Run the agent with the given query
async fn run_agent(
    cli: &Cli,
    config: &FileConfig,
    system_prompt: Option<&str>,
    query: &str,
) -> Result<()> {
    info!("Initializing Hermes agent");

    let client_config = config.merge_with(cli);
    let client = OpenAIClient::new(client_config);

    let registry = build_registry();

    let mut agent_config = AgentConfig::default();
    agent_config.model = config.model.clone().unwrap_or_else(|| cli.model.clone());
    agent_config.max_iterations = config.max_iterations.unwrap_or(cli.max_iterations);
    agent_config.tool_timeout =
        Duration::from_secs(config.tool_timeout.unwrap_or(cli.tool_timeout));

    if let Some(ref prompt) = config.system_prompt {
        agent_config.system_prompt = Some(prompt.clone());
    } else if let Some(ref prompt) = system_prompt {
        agent_config.system_prompt = Some(prompt.to_string());
    }

    // Create event channel for streaming output
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(100);

    let agent = HermesAgent::with_events(agent_config, client, registry, event_tx);

    // Spawn task to handle events
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::Thinking { content } => {
                    println!("\n[Thinking] {}", content);
                }
                AgentEvent::ToolStart { name, arguments } => {
                    println!("\n[Tool] Calling: {} with {}", name, arguments);
                }
                AgentEvent::ToolComplete { result } => {
                    if result.success {
                        println!("[Tool] Result: {}", result.content);
                    } else {
                        println!("[Tool] Error: {}", result.error.clone().unwrap_or_default());
                    }
                }
                AgentEvent::ToolError { name, error } => {
                    println!("[Tool] {} failed: {}", name, error);
                }
                AgentEvent::Content { text } => {
                    print!("{}", text);
                }
                AgentEvent::Done { message } => {
                    println!("\n\n[Done] Final response: {}", message.content);
                }
                AgentEvent::IterationComplete { iteration } => {
                    println!("\n[Iteration {} complete]", iteration);
                }
                AgentEvent::Error { error } => {
                    eprintln!("\n[Error] {}", error);
                }
            }
        }
    });

    // Run the agent
    match agent.run(query.to_string()).await {
        Ok(response) => {
            println!("\n\nAgent Response:\n{}", response.content);
        }
        Err(e) => {
            error!("Agent failed: {}", e);
            anyhow::bail!("Agent error: {}", e);
        }
    }

    Ok(())
}

/// List available tools
async fn list_tools(verbose: bool) -> Result<()> {
    let registry = build_registry();
    let tools = registry.get_schemas().await;

    if tools.is_empty() {
        println!("No tools registered.");
        return Ok(());
    }

    println!("Available tools ({} total):\n", tools.len());

    for (i, tool) in tools.iter().enumerate() {
        println!("{}. {}: {}", i + 1, tool.name, tool.description);

        if verbose {
            println!(
                "   Schema: {}",
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

    println!("Hermes-RS Chat Mode");
    println!("Type 'exit' or 'quit' to end the conversation.\n");

    let client_config = config.merge_with(cli);
    let client = OpenAIClient::new(client_config);
    let registry = build_registry();

    let mut agent_config = AgentConfig::default();
    agent_config.model = config.model.clone().unwrap_or_else(|| cli.model.clone());
    agent_config.max_iterations = config.max_iterations.unwrap_or(cli.max_iterations);
    agent_config.system_prompt = config
        .system_prompt
        .clone()
        .or(system_prompt.map(String::from));

    let agent = HermesAgent::new(agent_config, client, registry);

    loop {
        print!("You: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
            println!("Goodbye!");
            break;
        }

        if input.eq_ignore_ascii_case("clear") {
            agent.clear_history().await;
            println!("Conversation history cleared.\n");
            continue;
        }

        match agent.run(input.to_string()).await {
            Ok(response) => {
                println!("\nAssistant: {}\n", response.content);
            }
            Err(e) => {
                eprintln!("Error: {}\n", e);
            }
        }
    }

    Ok(())
}

/// Test a specific tool
async fn test_tool(
    _cli: &Cli,
    _config: &FileConfig,
    tool_name: &str,
    args: Option<&str>,
) -> Result<()> {
    let registry = build_registry();

    // Check if tool exists
    if !registry.contains(tool_name).await {
        anyhow::bail!("Tool '{}' not found", tool_name);
    }

    // Parse arguments
    let parsed_args: Value = if let Some(args_str) = args {
        serde_json::from_str(args_str).context("Failed to parse tool arguments as JSON")?
    } else {
        Value::Object(serde_json::Map::new())
    };

    println!("Testing tool: {}", tool_name);
    println!(
        "Arguments: {}",
        serde_json::to_string_pretty(&parsed_args).unwrap_or_default()
    );

    let result = registry
        .execute(
            tool_name,
            &format!("test_{}", tool_name),
            parsed_args,
            ToolContext::default(),
        )
        .await?;

    println!("\nResult:");
    println!("Success: {}", result.success);
    println!("Content: {}", result.content);
    if let Some(error) = result.error {
        println!("Error: {}", error);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    init_logging(cli.verbose, &cli.log_level);

    debug!("Hermes-RS CLI starting");
    debug!("Arguments: {:?}", cli);

    // Load configuration file if specified
    let config = if let Some(ref config_path) = cli.config {
        let contents = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        let ext = config_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("yaml");

        match ext {
            "json" => serde_json::from_str(&contents)?,
            _ => serde_yaml::from_str(&contents)?,
        }
    } else {
        // Check for default config locations
        let default_paths = vec![
            PathBuf::from("hermes.toml"),
            PathBuf::from(".hermes.toml"),
            dirs::config_dir()
                .map(|p| p.join("hermes").join("config.toml"))
                .unwrap_or_default(),
        ];

        let mut file_config = FileConfig {
            model: None,
            max_iterations: None,
            tool_timeout: None,
            system_prompt: None,
            api_key: None,
            base_url: None,
        };

        for path in default_paths {
            if path.exists() {
                let contents = std::fs::read_to_string(&path)?;
                file_config = serde_yaml::from_str(&contents).unwrap_or(file_config);
                info!("Loaded config from: {}", path.display());
                break;
            }
        }

        file_config
    };

    debug!("Effective config: {:?}", config);

    // Execute command
    match &cli.command {
        Commands::Run { system, query } => {
            let query = query
                .as_ref()
                .context("No query provided. Use --query or enter interactive mode.")?;
            run_agent(&cli, &config, system.as_deref(), query).await?;
        }
        Commands::Tools { verbose } => {
            list_tools(*verbose).await?;
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
