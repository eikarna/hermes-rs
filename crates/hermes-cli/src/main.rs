//! Hermes-RS CLI

mod autonomous;
mod tui;

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand};
use hermes_core::agent::{AgentConfig, AgentEvent, HermesAgent};
use hermes_core::auth::{default_auth_store_path, AuthMethod, AuthStore};
use hermes_core::client::{build_provider_for_kind, ClientConfig, LLMProvider, ProviderKind};
use hermes_core::config::{
    install_runtime_config, load_app_config, runtime_config, AppConfig, BehaviorSettings,
    LoggingSettings, McpServerConfig, McpTransportKind,
};
use hermes_core::mcp::McpManager;
use hermes_core::memory::MemoryManager;
use hermes_core::tools::{HermesTool, ToolContext, ToolRegistry};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::Level;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::tui::{LaunchMode, TuiApp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogTarget {
    Stderr,
    Sink,
    File,
}

#[derive(Debug, Parser)]
#[command(
    name = "hermes",
    about = "Hermes-RS: A high-performance ReAct agent framework",
    version
)]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[arg(short, long, global = true)]
    log_level: Option<String>,

    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[arg(long, global = true, env = "OPENAI_API_KEY")]
    api_key: Option<String>,

    #[arg(long, global = true, env = "OPENAI_BASE_URL")]
    base_url: Option<String>,

    #[arg(long, global = true)]
    model: Option<String>,

    #[arg(long, global = true)]
    max_iterations: Option<usize>,

    #[arg(long, global = true)]
    tool_timeout: Option<u64>,

    #[arg(long, global = true)]
    request_timeout: Option<u64>,

    #[arg(long, global = true)]
    context_window: Option<usize>,

    #[arg(long, global = true)]
    max_healing_attempts: Option<usize>,

    #[arg(long, global = true, action = ArgAction::SetTrue, conflicts_with = "no_stream")]
    stream: bool,

    #[arg(long = "no-stream", global = true, action = ArgAction::SetTrue, conflicts_with = "stream")]
    no_stream: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Run {
        #[arg(short, long)]
        system: Option<String>,

        #[arg(short, long)]
        query: Option<String>,

        #[arg(long, action = ArgAction::SetTrue)]
        autonomous: bool,
    },
    Autonomous {
        #[arg(short, long)]
        system: Option<String>,
    },
    Tools {
        #[arg(short, long)]
        verbose: bool,
    },
    Chat {
        #[arg(short, long)]
        system: Option<String>,
    },
    Test {
        #[arg()]
        tool_name: String,

        #[arg(short, long)]
        args: Option<String>,
    },
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommands {
    Providers,
    Login {
        #[arg()]
        provider: String,
    },
    SetApiKey {
        #[arg()]
        provider: String,

        #[arg(long)]
        name: Option<String>,

        #[arg(long = "env")]
        env_var: Option<String>,

        #[arg(long)]
        base_url: Option<String>,
    },
    SetBearerToken {
        #[arg()]
        provider: String,

        #[arg(long)]
        name: Option<String>,

        #[arg(long = "env")]
        env_var: Option<String>,

        #[arg(long)]
        base_url: Option<String>,
    },
    List,
    Logout {
        #[arg()]
        name: String,
    },
}

fn init_logging(
    verbose: bool,
    cli_log_level: Option<&str>,
    logging: &LoggingSettings,
    rich_output: bool,
) {
    let env_filter = if verbose {
        EnvFilter::new(format!("{}", Level::DEBUG))
    } else if let Some(level) = cli_log_level {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level))
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(logging.level.clone()))
    };

    let subscriber = tracing_subscriber::registry().with(env_filter);
    let layer = fmt::layer()
        .with_target(logging.with_target)
        .with_thread_ids(logging.with_thread_ids)
        .with_file(logging.with_file)
        .with_line_number(logging.with_line_number);

    match select_log_target(logging, rich_output) {
        LogTarget::File => {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(logging.log_file.as_ref().expect("log file should exist"))
                .expect("failed to open log file");
            let writer = Mutex::new(file);
            match logging.format.as_str() {
                "json" => subscriber
                    .with(layer.with_writer(writer).with_ansi(false).json())
                    .init(),
                "compact" => subscriber
                    .with(layer.with_writer(writer).with_ansi(false).compact())
                    .init(),
                _ => subscriber
                    .with(layer.with_writer(writer).with_ansi(false).pretty())
                    .init(),
            }
        }
        LogTarget::Sink => match logging.format.as_str() {
            "json" => subscriber
                .with(layer.with_writer(io::sink).with_ansi(false).json())
                .init(),
            "compact" => subscriber
                .with(layer.with_writer(io::sink).with_ansi(false).compact())
                .init(),
            _ => subscriber
                .with(layer.with_writer(io::sink).with_ansi(false).pretty())
                .init(),
        },
        LogTarget::Stderr => match logging.format.as_str() {
            "json" => subscriber.with(layer.json()).init(),
            "compact" => subscriber.with(layer.compact()).init(),
            _ => subscriber.with(layer.pretty()).init(),
        },
    }
}

fn select_log_target(logging: &LoggingSettings, rich_output: bool) -> LogTarget {
    if logging.log_file.is_some() {
        LogTarget::File
    } else if rich_output {
        LogTarget::Sink
    } else {
        LogTarget::Stderr
    }
}

fn apply_cli_overrides(cli: &Cli, config: &mut AppConfig) {
    if let Some(api_key) = &cli.api_key {
        config.client.api_key = Some(api_key.clone());
    }
    if let Some(base_url) = &cli.base_url {
        config.client.base_url = base_url.clone();
    }
    if let Some(model) = &cli.model {
        config.agent.model = model.clone();
    }
    if let Some(max_iterations) = cli.max_iterations {
        config.agent.max_iterations = max_iterations;
    }
    if let Some(timeout) = cli.tool_timeout {
        config.agent.tool_timeout_secs = timeout;
    }
    if let Some(timeout) = cli.request_timeout {
        config.agent.request_timeout_secs = timeout;
        config.client.timeout_secs = timeout;
    }
    if let Some(window) = cli.context_window {
        config.agent.context_window = window;
        config.client.max_context_length = window;
    }
    if let Some(healing) = cli.max_healing_attempts {
        config.agent.max_healing_attempts = healing;
    }
    if cli.stream {
        config.agent.stream = true;
    }
    if cli.no_stream {
        config.agent.stream = false;
    }
}

fn client_config(config: &AppConfig) -> Result<(ProviderKind, ClientConfig)> {
    let kind = ProviderKind::from_name(&config.client.provider).ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported client provider '{}'. Expected one of: openai, anthropic, ollama, openrouter.",
            config.client.provider
        )
    })?;
    let resolved = hermes_core::client::resolve_provider_settings(&config.client)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let client = ClientConfig {
        base_url: resolved.config.base_url,
        api_key: resolved.config.api_key,
        timeout: resolved.config.timeout,
        max_context_length: resolved.max_context_length,
    };
    if let Some(auth_ref) = config.client.auth_ref.as_deref() {
        let store = AuthStore::load_default()?;
        let client = apply_auth_profile_to_client(client, &store, auth_ref)?;
        return Ok((infer_provider_from_base_url(&client.base_url), client));
    }
    Ok((kind, client))
}

fn infer_provider_from_base_url(base_url: &str) -> ProviderKind {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    if normalized.contains(".anthropic.com") {
        ProviderKind::Anthropic
    } else if normalized.contains("openrouter.ai") {
        ProviderKind::Openrouter
    } else if normalized.contains("localhost:11434") || normalized.contains("127.0.0.1:11434") {
        ProviderKind::Ollama
    } else {
        ProviderKind::Openai
    }
}

fn runtime_client(config: &AppConfig) -> Result<Arc<dyn LLMProvider>> {
    let (kind, client) = client_config(config)?;
    build_provider_for_kind(kind, client).map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn apply_auth_profile_to_client(
    mut client: ClientConfig,
    store: &AuthStore,
    auth_ref: &str,
) -> Result<ClientConfig> {
    let profile = store.profiles.get(auth_ref).ok_or_else(|| {
        anyhow::anyhow!(
            "Missing required configuration: auth profile '{}'",
            auth_ref
        )
    })?;
    let trusted_base_url = profile
        .base_url
        .clone()
        .or_else(|| match profile.method {
            AuthMethod::ApiKey if is_openai_provider(&profile.provider) => {
                Some(hermes_core::config::ClientSettings::default().base_url)
            }
            AuthMethod::ApiKey => None,
            AuthMethod::BearerToken => None,
        })
        .ok_or_else(|| anyhow::anyhow!("Auth profile '{}' requires a base URL", auth_ref))?;
    let default_base_url = hermes_core::config::ClientSettings::default().base_url;
    if client.base_url != default_base_url && client.base_url != trusted_base_url {
        anyhow::bail!(
            "Auth profile '{}' is bound to '{}'; refusing to send credentials to '{}'",
            auth_ref,
            trusted_base_url,
            client.base_url
        );
    }
    match profile.method {
        AuthMethod::ApiKey => {
            client.api_key = Some(store.resolve_api_key(auth_ref)?);
        }
        AuthMethod::BearerToken => {
            client.api_key = Some(store.resolve_auth_token(auth_ref)?);
        }
    }
    client.base_url = trusted_base_url;
    Ok(client)
}

fn agent_config(
    config: &AppConfig,
    behavior: &BehaviorSettings,
    system_prompt: Option<&str>,
) -> AgentConfig {
    let mut agent = AgentConfig::from(behavior);
    if let Some(prompt) = system_prompt {
        agent.system_prompt = Some(prompt.to_string());
    }
    agent.request_timeout = Duration::from_secs(config.agent.request_timeout_secs);
    agent
}

pub(crate) async fn build_registry(
    config: &AppConfig,
    mcp_manager: &mut McpManager,
    client: &Arc<dyn LLMProvider>,
    model: &str,
) -> Result<ToolRegistry> {
    let registry = ToolRegistry::new(Duration::from_secs(config.tools.registry_timeout_secs));
    hermes_core::tools::register_builtin_tools_with_provider_sub_agent(
        &registry,
        client.clone(),
        model,
    )
    .await?;
    registry.register(EchoTool::new()).await?;
    registry.register(CalculatorTool::new()).await?;

    if config.mcp.autoload {
        for server in config.mcp.servers.iter().filter(|server| server.enabled) {
            if mcp_manager.get(&server.name).is_none() {
                connect_mcp_server(mcp_manager, server).await?;
            }
        }

        for tool in mcp_manager.get_all_tools().await {
            registry.register(tool).await?;
        }
    }

    Ok(registry)
}

async fn connect_mcp_server(mcp_manager: &mut McpManager, server: &McpServerConfig) -> Result<()> {
    match server.transport {
        McpTransportKind::Http => {
            let url = server
                .url
                .clone()
                .context("Configured HTTP MCP server is missing a URL")?;
            mcp_manager
                .add_server(server.name.clone(), url, server.auth_token.clone())
                .await?;
        }
        McpTransportKind::Stdio => {
            let command = server
                .command
                .clone()
                .context("Configured stdio MCP server is missing a command")?;
            mcp_manager
                .add_stdio_server(
                    server.name.clone(),
                    command,
                    server.args.clone(),
                    server.env.clone(),
                )
                .await?;
        }
    }
    Ok(())
}

pub(crate) async fn create_runtime_agent(
    config: &AppConfig,
    behavior: &BehaviorSettings,
    system_prompt: Option<&str>,
    event_tx: mpsc::Sender<AgentEvent>,
    mcp_manager: &mut McpManager,
) -> Result<HermesAgent> {
    let client = runtime_client(config)?;
    let registry = build_registry(config, mcp_manager, &client, &behavior.model).await?;
    let agent_config = agent_config(config, behavior, system_prompt);
    let memory_manager = load_repo_memory_manager().await?;
    Ok(
        HermesAgent::with_provider_events(agent_config, client, registry, event_tx)
            .with_memory_manager(memory_manager),
    )
}

async fn create_agent_without_events(
    config: &AppConfig,
    system_prompt: Option<&str>,
    mcp_manager: &mut McpManager,
) -> Result<HermesAgent> {
    let client = runtime_client(config)?;
    let registry = build_registry(config, mcp_manager, &client, &config.agent.model).await?;
    let agent_config = agent_config(config, &config.agent, system_prompt);
    let memory_manager = load_repo_memory_manager().await?;
    Ok(
        HermesAgent::new_with_provider(agent_config, client, registry)
            .with_memory_manager(memory_manager),
    )
}

async fn load_repo_memory_manager() -> Result<MemoryManager> {
    let storage_dir = std::env::current_dir().context("Failed to determine current directory")?;
    let memory_manager = load_memory_manager(storage_dir).await?;
    // Curator pass runs in the background: decay/prune/distill without
    // blocking startup. Failures are logged and non-fatal.
    let config = runtime_config();
    let policy = config.curator.clone();
    let skills_dir = config.skills.root_dir.clone();
    let curated = memory_manager.clone();
    tokio::spawn(async move {
        match hermes_core::curator::curate(&curated, &skills_dir, &policy).await {
            Ok(report) if !report.is_empty() => {
                tracing::info!(?report, "Curator pass complete");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "Curator pass failed");
            }
        }
    });
    Ok(memory_manager)
}

async fn load_memory_manager(storage_dir: PathBuf) -> Result<MemoryManager> {
    let memory_manager = MemoryManager::with_storage_dir(storage_dir);
    memory_manager
        .load_from_disk()
        .await
        .context("Failed to load long-term memory")?;
    Ok(memory_manager)
}

async fn run_non_tui(config: &AppConfig, system_prompt: Option<&str>, query: &str) -> Result<()> {
    let mut mcp_manager = McpManager::new();
    let agent = create_agent_without_events(config, system_prompt, &mut mcp_manager).await?;
    let response = agent.run(query.to_string()).await?;
    println!("{}", response.content);
    Ok(())
}

async fn chat_non_tui(config: &AppConfig, system_prompt: Option<&str>) -> Result<()> {
    let mut mcp_manager = McpManager::new();
    let agent = create_agent_without_events(config, system_prompt, &mut mcp_manager).await?;

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
            break;
        }
        if input.eq_ignore_ascii_case("clear") {
            agent.clear_history().await;
            println!("Conversation cleared.");
            continue;
        }

        match agent.run(input.to_string()).await {
            Ok(response) => println!("Assistant: {}\n", response.content),
            Err(error) => eprintln!("Error: {}\n", error),
        }
    }

    Ok(())
}

async fn list_tools(config: &AppConfig, verbose: bool) -> Result<()> {
    let mut mcp_manager = McpManager::new();
    let client = runtime_client(config)?;
    let registry = build_registry(config, &mut mcp_manager, &client, &config.agent.model).await?;
    let tools = registry.get_schemas().await;

    for tool in tools {
        println!("{}: {}", tool.name, tool.description);
        if verbose {
            println!("{}", serde_json::to_string_pretty(&tool.parameters)?);
        }
    }

    Ok(())
}

async fn test_tool(config: &AppConfig, tool_name: &str, args: Option<&str>) -> Result<()> {
    let mut mcp_manager = McpManager::new();
    let client = runtime_client(config)?;
    let registry = build_registry(config, &mut mcp_manager, &client, &config.agent.model).await?;
    let parsed_args: Value = if let Some(args) = args {
        serde_json::from_str(args).context("Failed to parse tool arguments as JSON")?
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

    println!("success: {}", result.success);
    println!("content: {}", result.content);
    if let Some(error) = result.error {
        println!("error: {}", error);
    }

    Ok(())
}

fn handle_auth_command(command: &AuthCommands) -> Result<()> {
    match command {
        AuthCommands::Providers => print_auth_providers(),
        AuthCommands::Login { provider } => {
            let provider = canonical_provider(provider)?;
            print_auth_login_guidance(provider);
            anyhow::bail!(
                "Hermes does not run '{}' login flows yet; use the listed external credential source and create an auth profile with set-api-key or set-bearer-token.",
                provider.name
            );
        }
        AuthCommands::SetApiKey {
            provider,
            name,
            env_var,
            base_url,
        } => {
            let provider = canonical_provider(provider)?;
            if provider.name != "OpenAI"
                && base_url.as_deref().map(str::trim).unwrap_or("").is_empty()
            {
                anyhow::bail!(
                    "Provider '{}' API-key profiles require --base-url so credentials are bound to the intended endpoint.",
                    provider.name
                );
            }
            let profile_name = name
                .clone()
                .unwrap_or_else(|| format!("{}-default", provider.slug));
            let env_var = env_var
                .clone()
                .unwrap_or_else(|| provider.api_key_env.to_string());
            let mut store = AuthStore::load_default()?;
            store.upsert_api_key_env_profile(
                profile_name.clone(),
                provider.name.to_string(),
                env_var.clone(),
                base_url.clone(),
            )?;
            store.save_default()?;
            println!(
                "Saved auth profile '{}' for provider '{}' using env:{}",
                profile_name, provider.name, env_var
            );
            println!("Auth metadata: {}", default_auth_store_path().display());
        }
        AuthCommands::SetBearerToken {
            provider,
            name,
            env_var,
            base_url,
        } => {
            let provider = canonical_provider(provider)?;
            let default_env_var = provider.bearer_env;
            let profile_name = name
                .clone()
                .unwrap_or_else(|| format!("{}-default", provider.slug));
            let env_var = match (env_var, default_env_var) {
                (Some(env_var), _) => env_var.clone(),
                (None, Some(default_env_var)) => default_env_var.to_string(),
                (None, None) => anyhow::bail!(
                    "Provider '{}' does not have a default bearer-token OAuth flow. Use --env with a documented bearer token source if you know what you are doing.",
                    provider.name
                ),
            };
            let mut store = AuthStore::load_default()?;
            store.upsert_bearer_token_env_profile(
                profile_name.clone(),
                provider.name.to_string(),
                env_var.clone(),
                base_url.clone(),
            )?;
            store.save_default()?;
            println!(
                "Saved bearer auth profile '{}' for provider '{}' using env:{}",
                profile_name, provider.name, env_var
            );
            println!("Auth metadata: {}", default_auth_store_path().display());
        }
        AuthCommands::List => {
            let store = AuthStore::load_default()?;
            if store.profiles.is_empty() {
                println!("No auth profiles configured.");
                return Ok(());
            }
            for (name, profile) in store.profiles {
                let secret_ref = profile
                    .resolved_env_var()
                    .map(|env_var| display_auth_field(&format!("env:{}", env_var)))
                    .unwrap_or_else(|| "unsupported".to_string());
                let base_url = profile
                    .base_url
                    .as_deref()
                    .map(display_auth_field)
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{}\tprovider={}\tmethod={:?}\tsecret={}\tbase_url={}",
                    display_auth_field(&name),
                    display_auth_field(&profile.provider),
                    profile.method,
                    secret_ref,
                    base_url
                );
            }
        }
        AuthCommands::Logout { name } => {
            let mut store = AuthStore::load_default()?;
            if !store.remove_profile(name) {
                anyhow::bail!("Auth profile '{}' was not found", name);
            }
            store.save_default()?;
            println!("Removed auth profile '{}'", name);
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderInfo {
    name: &'static str,
    slug: &'static str,
    aliases: &'static [&'static str],
    api_key_env: &'static str,
    api_key_envs: &'static [&'static str],
    bearer_env: Option<&'static str>,
    documented_auth: &'static [&'static str],
    hermes_sources: &'static [&'static str],
    notes: &'static str,
    login_guidance: &'static [&'static str],
}

const AUTH_PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        name: "Google",
        slug: "google",
        aliases: &[
            "google",
            "gemini",
            "google-gemini",
            "google ai studio",
            "google-ai-studio",
            "vertex",
            "vertex-ai",
            "google vertex ai",
        ],
        api_key_env: "GOOGLE_API_KEY",
        api_key_envs: &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        bearer_env: Some("GOOGLE_OAUTH_ACCESS_TOKEN"),
        documented_auth: &["API key", "OAuth/ADC bearer token", "service account/ADC"],
        hermes_sources: &["GOOGLE_API_KEY", "GEMINI_API_KEY", "GOOGLE_OAUTH_ACCESS_TOKEN"],
        notes: "Direct OAuth requires a Google OAuth client ID; ADC can be managed by gcloud.",
        login_guidance: &[
            "For API-key auth, create a Google AI Studio API key and run `hermes auth set-api-key Google --env GOOGLE_API_KEY --base-url <google-endpoint>`.",
            "For OAuth/ADC, run `gcloud auth application-default login`, export an access token through your own refresh workflow, then run `hermes auth set-bearer-token Google --env GOOGLE_OAUTH_ACCESS_TOKEN --base-url <google-endpoint>`.",
        ],
    },
    ProviderInfo {
        name: "GitHub Copilot",
        slug: "github-copilot",
        aliases: &["github-copilot", "github copilot", "copilot", "github"],
        api_key_env: "COPILOT_GITHUB_TOKEN",
        api_key_envs: &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
        bearer_env: Some("COPILOT_GITHUB_TOKEN"),
        documented_auth: &["OAuth device flow", "supported GitHub token", "GitHub CLI fallback"],
        hermes_sources: &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"],
        notes: "Hermes can reference tokens now; running Copilot login directly is future work.",
        login_guidance: &[
            "Run the official Copilot or GitHub CLI login flow, or provide a supported token in COPILOT_GITHUB_TOKEN, GH_TOKEN, or GITHUB_TOKEN.",
            "Then run `hermes auth set-bearer-token GitHub-Copilot --env COPILOT_GITHUB_TOKEN --base-url <copilot-compatible-endpoint>` only for endpoints documented to accept that token.",
        ],
    },
    ProviderInfo {
        name: "OpenAI",
        slug: "openai",
        aliases: &["openai"],
        api_key_env: "OPENAI_API_KEY",
        api_key_envs: &["OPENAI_API_KEY"],
        bearer_env: None,
        documented_auth: &[
            "API key",
            "ChatGPT/Codex browser login",
            "ChatGPT/Codex device login",
        ],
        hermes_sources: &["OPENAI_API_KEY"],
        notes: "Hermes supports API-key metadata now; Codex/ChatGPT OAuth is documented but not wired into Hermes runtime yet.",
        login_guidance: &[
            "For current Hermes runtime use, create an OpenAI API key and run `hermes auth set-api-key OpenAI --env OPENAI_API_KEY`.",
            "OpenAI Codex/ChatGPT browser and device login are documented provider flows, but Hermes does not consume Codex account tokens yet.",
        ],
    },
    ProviderInfo {
        name: "Anthropic",
        slug: "anthropic",
        aliases: &[
            "anthropic",
            "claude",
            "anthropic console",
        ],
        api_key_env: "ANTHROPIC_API_KEY",
        api_key_envs: &["ANTHROPIC_API_KEY"],
        bearer_env: None,
        documented_auth: &[
            "Claude account login",
            "Anthropic Console API key",
            "Team/Enterprise account",
            "Vertex AI",
            "Amazon Bedrock",
            "Microsoft Foundry",
        ],
        hermes_sources: &["ANTHROPIC_API_KEY"],
        notes: "Hermes supports API-key metadata now; Claude account and cloud-provider flows need provider-specific clients before runtime use.",
        login_guidance: &[
            "For current Hermes runtime use, create an Anthropic Console API key and run `hermes auth set-api-key Anthropic --env ANTHROPIC_API_KEY --base-url <anthropic-compatible-endpoint>`.",
            "Claude account login, Team/Enterprise, Vertex AI, Amazon Bedrock, and Microsoft Foundry need provider-specific clients before Hermes can use those credentials directly.",
        ],
    },
];

fn canonical_provider(input: &str) -> Result<&'static ProviderInfo> {
    let normalized = input.trim().to_ascii_lowercase();
    AUTH_PROVIDERS
        .iter()
        .find(|provider| provider.aliases.contains(&normalized.as_str()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Unsupported provider '{}'. Run `hermes auth providers` for supported names.",
                input
            )
        })
}

fn print_auth_providers() {
    println!("Supported auth providers:");
    for provider in AUTH_PROVIDERS {
        println!(
            "{}\tslug={}\taliases={}\tapi_key_envs={}\tbearer_env={}\tdocumented_auth={}\thermes_sources={}\tnotes={}",
            provider.name,
            provider.slug,
            provider.aliases.join(","),
            provider.api_key_envs.join(","),
            provider.bearer_env.unwrap_or("-"),
            provider.documented_auth.join(","),
            provider.hermes_sources.join(","),
            provider.notes
        );
    }
}

fn print_auth_login_guidance(provider: &ProviderInfo) {
    println!("{} login is not enabled in Hermes yet.", provider.name);
    println!("Documented auth: {}", provider.documented_auth.join(", "));
    println!(
        "Hermes-supported sources: {}",
        provider.hermes_sources.join(", ")
    );
    for line in provider.login_guidance {
        println!("- {}", line);
    }
}

fn is_openai_provider(provider: &str) -> bool {
    provider.eq_ignore_ascii_case("OpenAI") || provider.eq_ignore_ascii_case("openai")
}

fn display_auth_field(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { '?' } else { ch })
        .collect()
}

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
        if let Some(msg) = args.get("message").and_then(|value| value.as_str()) {
            hermes_core::tools::ToolResult::success("echo", serde_json::json!({ "echoed": msg }))
        } else {
            hermes_core::tools::ToolResult::error("echo", "Missing 'message' argument")
        }
    }
}

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
        "Perform a calculation. Supports add, subtract, multiply, and divide."
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
        let operation = args
            .get("operation")
            .and_then(|value| value.as_str())
            .unwrap_or("add");
        let a = args
            .get("a")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        let b = args
            .get("b")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);

        let result = match operation {
            "add" | "+" => a + b,
            "subtract" | "-" => a - b,
            "multiply" | "*" | "x" => a * b,
            "divide" | "/" => {
                if b == 0.0 {
                    return hermes_core::tools::ToolResult::error("calculate", "Division by zero");
                }
                a / b
            }
            _ => {
                return hermes_core::tools::ToolResult::error(
                    "calculate",
                    format!("Unknown operation: {}", operation),
                )
            }
        };

        hermes_core::tools::ToolResult::success(
            "calculate",
            serde_json::json!({
                "operation": operation,
                "operand_a": a,
                "operand_b": b,
                "result": result
            }),
        )
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut loaded = load_app_config(cli.config.as_deref())?;
    loaded.config.apply_env_overrides()?;
    apply_cli_overrides(&cli, &mut loaded.config);
    install_runtime_config(loaded.config.clone());

    init_logging(
        cli.verbose,
        cli.log_level.as_deref(),
        &loaded.config.logging,
        loaded.config.tui.rich_output,
    );

    match &cli.command {
        Commands::Run {
            system,
            query,
            autonomous,
        } => {
            if *autonomous {
                if query.is_some() {
                    anyhow::bail!(
                        "Do not combine 'run --autonomous' with '--query'. Autonomous mode reads TODO.md from the workspace."
                    );
                }
                autonomous::run_autonomous(loaded.config.clone(), system.clone()).await?;
                return Ok(());
            }
            let query = query
                .as_ref()
                .context("No query provided. Use --query or start chat mode.")?;
            if loaded.config.tui.rich_output {
                TuiApp::enter(
                    loaded.config.clone(),
                    system.clone(),
                    LaunchMode::Query(query.clone()),
                )
                .await?
                .run()
                .await?;
            } else {
                run_non_tui(&loaded.config, system.as_deref(), query).await?;
            }
        }
        Commands::Chat { system } => {
            if loaded.config.tui.rich_output {
                TuiApp::enter(loaded.config.clone(), system.clone(), LaunchMode::Landing)
                    .await?
                    .run()
                    .await?;
            } else {
                chat_non_tui(&loaded.config, system.as_deref()).await?;
            }
        }
        Commands::Tools { verbose } => {
            list_tools(&loaded.config, *verbose).await?;
        }
        Commands::Autonomous { system } => {
            autonomous::run_autonomous(loaded.config.clone(), system.clone()).await?;
        }
        Commands::Test { tool_name, args } => {
            test_tool(&loaded.config, tool_name, args.as_deref()).await?;
        }
        Commands::Auth { command } => {
            handle_auth_command(command)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::auth::AuthProfile;

    #[test]
    fn infers_provider_wire_format_from_auth_profile_endpoint() {
        assert_eq!(
            infer_provider_from_base_url("https://api.anthropic.com/v1"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            infer_provider_from_base_url("https://openrouter.ai/api/v1"),
            ProviderKind::Openrouter
        );
        assert_eq!(
            infer_provider_from_base_url("http://localhost:11434/v1"),
            ProviderKind::Ollama
        );
        assert_eq!(
            infer_provider_from_base_url("https://example.invalid/v1"),
            ProviderKind::Openai
        );
    }

    #[test]
    fn rich_tui_without_log_file_uses_sink() {
        let logging = LoggingSettings::default();
        assert_eq!(select_log_target(&logging, true), LogTarget::Sink);
    }

    #[test]
    fn log_file_overrides_sink() {
        let logging = LoggingSettings {
            log_file: Some("hermes.log".to_string()),
            ..Default::default()
        };
        assert_eq!(select_log_target(&logging, true), LogTarget::File);
    }

    #[tokio::test]
    async fn load_memory_manager_reads_existing_memory_file() {
        let dir =
            std::env::temp_dir().join(format!("hermes_cli_memory_load_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let seed = MemoryManager::with_storage_dir(dir.clone());
        seed.store(
            hermes_core::memory::MemoryBlock::new("cli_fact", "fact", "Loaded memory fact")
                .importance(90),
        )
        .await;

        let loaded = load_memory_manager(dir.clone()).await.unwrap();

        assert_eq!(loaded.search("Loaded memory").await.len(), 1);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn autonomous_subcommand_parses() {
        let cli = Cli::try_parse_from(["hermes", "autonomous"]).unwrap();
        assert!(matches!(cli.command, Commands::Autonomous { .. }));
    }

    #[test]
    fn run_autonomous_flag_parses() {
        let cli = Cli::try_parse_from(["hermes", "run", "--autonomous"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Run {
                autonomous: true,
                ..
            }
        ));
    }

    #[test]
    fn auth_set_api_key_subcommand_parses() {
        let cli = Cli::try_parse_from([
            "hermes",
            "auth",
            "set-api-key",
            "openai",
            "--name",
            "openai-default",
            "--env",
            "OPENAI_API_KEY",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Auth {
                command: AuthCommands::SetApiKey { .. }
            }
        ));
    }

    #[test]
    fn auth_set_bearer_token_subcommand_parses() {
        let cli = Cli::try_parse_from([
            "hermes",
            "auth",
            "set-bearer-token",
            "google-gemini",
            "--name",
            "google-default",
            "--env",
            "GOOGLE_OAUTH_ACCESS_TOKEN",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Auth {
                command: AuthCommands::SetBearerToken { .. }
            }
        ));
    }

    #[test]
    fn auth_providers_subcommand_parses() {
        let cli = Cli::try_parse_from(["hermes", "auth", "providers"]).unwrap();

        assert!(matches!(
            cli.command,
            Commands::Auth {
                command: AuthCommands::Providers
            }
        ));
    }

    #[test]
    fn auth_login_subcommand_parses() {
        let cli = Cli::try_parse_from(["hermes", "auth", "login", "OpenAI"]).unwrap();

        assert!(matches!(
            cli.command,
            Commands::Auth {
                command: AuthCommands::Login { .. }
            }
        ));
    }

    #[test]
    fn auth_login_guidance_does_not_create_profile() {
        let result = handle_auth_command(&AuthCommands::Login {
            provider: "OpenAI".to_string(),
        });

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("does not run 'OpenAI' login flows yet"));
    }

    #[test]
    fn canonical_provider_names_and_defaults_are_supported() {
        let google = canonical_provider("gemini").unwrap();
        assert_eq!(google.name, "Google");
        assert_eq!(google.slug, "google");
        assert_eq!(google.api_key_env, "GOOGLE_API_KEY");
        assert!(google.api_key_envs.contains(&"GEMINI_API_KEY"));
        assert_eq!(google.bearer_env, Some("GOOGLE_OAUTH_ACCESS_TOKEN"));
        assert!(google.documented_auth.contains(&"OAuth/ADC bearer token"));
        assert!(google
            .login_guidance
            .iter()
            .any(|line| line.contains("gcloud")));

        let copilot = canonical_provider("copilot").unwrap();
        assert_eq!(copilot.name, "GitHub Copilot");
        assert_eq!(copilot.bearer_env, Some("COPILOT_GITHUB_TOKEN"));
        assert!(copilot.api_key_envs.contains(&"GH_TOKEN"));
        assert_eq!(
            canonical_provider("GitHub Copilot").unwrap().slug,
            "github-copilot"
        );

        let openai = canonical_provider("openai").unwrap();
        assert_eq!(openai.name, "OpenAI");
        assert!(openai
            .documented_auth
            .contains(&"ChatGPT/Codex device login"));
        assert!(openai
            .login_guidance
            .iter()
            .any(|line| line.contains("does not consume Codex account tokens")));
        assert!(canonical_provider("codex").is_err());
        assert!(!openai.hermes_sources.contains(&"CODEX_ACCESS_TOKEN"));
        assert_eq!(openai.bearer_env, None);

        let anthropic = canonical_provider("claude").unwrap();
        assert_eq!(anthropic.name, "Anthropic");
        assert_eq!(anthropic.api_key_env, "ANTHROPIC_API_KEY");
        assert!(anthropic.documented_auth.contains(&"Amazon Bedrock"));
        assert!(canonical_provider("claude-code").is_err());
        assert!(canonical_provider("microsoft foundry").is_err());
        assert_eq!(anthropic.bearer_env, None);
    }

    #[test]
    fn auth_profile_takes_precedence_over_configured_api_key() {
        let mut store = AuthStore::default();
        let old_profile_key = std::env::var("HERMES_TEST_PROFILE_KEY_PRECEDENCE").ok();
        std::env::set_var("HERMES_TEST_PROFILE_KEY_PRECEDENCE", "profile-key");
        store
            .upsert_api_key_env_profile(
                "openai-default",
                "openai",
                "HERMES_TEST_PROFILE_KEY_PRECEDENCE",
                None,
            )
            .unwrap();
        let client = ClientConfig {
            api_key: Some("configured-key".to_string()),
            ..ClientConfig::default()
        };

        let resolved = apply_auth_profile_to_client(client, &store, "openai-default").unwrap();

        assert_eq!(resolved.api_key.as_deref(), Some("profile-key"));

        match old_profile_key {
            Some(value) => std::env::set_var("HERMES_TEST_PROFILE_KEY_PRECEDENCE", value),
            None => std::env::remove_var("HERMES_TEST_PROFILE_KEY_PRECEDENCE"),
        }
    }

    #[test]
    fn auth_profile_base_url_applies_when_default_base_url_is_unset() {
        let mut store = AuthStore::default();
        let old_profile_key = std::env::var("HERMES_TEST_PROFILE_KEY_BASE_URL").ok();
        std::env::set_var("HERMES_TEST_PROFILE_KEY_BASE_URL", "profile-key");
        store
            .upsert_api_key_env_profile(
                "local-default",
                "local",
                "HERMES_TEST_PROFILE_KEY_BASE_URL",
                Some("http://127.0.0.1:11434/v1".to_string()),
            )
            .unwrap();
        let client = ClientConfig {
            api_key: Some("configured-key".to_string()),
            ..ClientConfig::default()
        };

        let resolved = apply_auth_profile_to_client(client, &store, "local-default").unwrap();

        assert_eq!(resolved.api_key.as_deref(), Some("profile-key"));
        assert_eq!(resolved.base_url, "http://127.0.0.1:11434/v1");

        match old_profile_key {
            Some(value) => std::env::set_var("HERMES_TEST_PROFILE_KEY_BASE_URL", value),
            None => std::env::remove_var("HERMES_TEST_PROFILE_KEY_BASE_URL"),
        }
    }

    #[test]
    fn auth_profile_rejects_untrusted_base_url_override() {
        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile("openai-default", "openai", "OPENAI_API_KEY", None)
            .unwrap();
        let client = ClientConfig {
            base_url: "https://attacker.example/v1".to_string(),
            api_key: Some("configured-key".to_string()),
            ..ClientConfig::default()
        };

        let result = apply_auth_profile_to_client(client, &store, "openai-default");

        assert!(result.is_err());
    }

    #[test]
    fn non_openai_api_key_profile_requires_base_url() {
        let mut store = AuthStore::default();
        store
            .upsert_api_key_env_profile("google-default", "Google", "GOOGLE_API_KEY", None)
            .unwrap();
        let client = ClientConfig {
            api_key: Some("configured-key".to_string()),
            ..ClientConfig::default()
        };

        let result = apply_auth_profile_to_client(client, &store, "google-default");

        assert!(result.is_err());
    }

    #[test]
    fn bearer_profile_overrides_ambient_api_key_for_bound_endpoint() {
        let mut store = AuthStore::default();
        store
            .upsert_bearer_token_env_profile(
                "google-default",
                "google-gemini",
                "GOOGLE_OAUTH_ACCESS_TOKEN",
                Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            )
            .unwrap();
        let old_token = std::env::var("GOOGLE_OAUTH_ACCESS_TOKEN").ok();
        std::env::set_var("GOOGLE_OAUTH_ACCESS_TOKEN", "google-token");
        let client = ClientConfig {
            api_key: Some("openai-key".to_string()),
            ..ClientConfig::default()
        };

        let resolved = apply_auth_profile_to_client(client, &store, "google-default").unwrap();

        assert_eq!(resolved.api_key.as_deref(), Some("google-token"));
        assert_eq!(
            resolved.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
        if let Some(old_token) = old_token {
            std::env::set_var("GOOGLE_OAUTH_ACCESS_TOKEN", old_token);
        } else {
            std::env::remove_var("GOOGLE_OAUTH_ACCESS_TOKEN");
        }
    }

    #[test]
    fn bearer_profile_without_base_url_is_rejected_even_if_loaded_from_disk() {
        let mut store = AuthStore::default();
        store.profiles.insert(
            "broken-bearer".to_string(),
            AuthProfile {
                provider: "google-gemini".to_string(),
                method: AuthMethod::BearerToken,
                base_url: None,
                secret_ref: "env:GOOGLE_OAUTH_ACCESS_TOKEN".to_string(),
                disabled: false,
            },
        );
        let client = ClientConfig {
            api_key: Some("openai-key".to_string()),
            ..ClientConfig::default()
        };

        let result = apply_auth_profile_to_client(client, &store, "broken-bearer");

        assert!(result.is_err());
    }

    #[test]
    fn auth_list_fields_escape_control_characters() {
        assert_eq!(display_auth_field("good\nspoof"), "good?spoof");
    }
}
