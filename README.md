![Hermes-RS](assets/banner.png)

A high-performance Rust implementation of the Hermes-Agent orchestration loop for LLM-driven tool execution.

## Features

- **Streaming-First Architecture**: Detect and execute tool calls incrementally from partial LLM outputs
- **Tolerant XML Parser**: Handle malformed tags and unclosed JSON with state-machine parsing
- **Early Tool Detection**: Initiate tool execution as soon as `</tool_call>` is detected
- **Self-Healing**: Automatically re-prompt LLM with error context on failures
- **Dynamic Schema Generation**: Automatically generate JSON Schema from Rust structs
- **Shared TOML Configuration**: One runtime config model across `hermes-cli` and `hermes-core`
- **Ratatui TUI**: Prompt-first landing view, responsive workspace panes, reasoning display, MCP/Skills/Behavior management
- **Structured Logging**: Comprehensive observability via the `tracing` crate

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Hermes-RS                           │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────────┐  │
│  │ OpenAI      │  │ XMLParser    │  │ ToolRegistry      │  │
│  │ Client      │  │ (Tolerant)   │  │ & Execution       │  │
│  └─────────────┘  └──────────────┘  └────────────────────┘  │
│  ┌─────────────────────────────────────────────────────────┐│
│  │            Orchestration Loop (ReAct)                   ││
│  │  Think → Plan → Execute Tools → Observe → Respond       ││
│  └─────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────┘
```

## Installation

```bash
# Build from source
cargo build --release

# Or install the CLI crate directly
cargo install --path crates/hermes-cli
```

Tagged releases publish per-platform binaries automatically in the repository's GitHub Releases tab.

## Quick Start

```bash
# Set your API key
export OPENAI_API_KEY=your_api_key_here   # PowerShell: $env:OPENAI_API_KEY="..."

# Start the prompt-first TUI
hermes chat

# Run a one-shot query
hermes run --query "What is 2 + 2?"

# List available tools
hermes tools

# Test a specific tool
hermes test echo --args '{"message": "Hello, World!"}'
```

## Screenshots

Prompt-first landing screen:

![Hermes landing screen](assets/main.png)

Workspace session with conversation, reasoning, and activity panes:

![Hermes workspace chat screen](assets/chat.png)

## Configuration

Hermes reads configuration in this order:

1. `--config <path>`
2. `./hermes.toml`
3. `./.hermes.toml`
4. OS config directory (for example `~/.config/hermes/config.toml` on Linux)
5. Environment variables
6. CLI flags

Start from the checked-in example file:

```bash
cp hermes.example.toml hermes.toml
```

Configuration is TOML, not YAML. Example:

```toml
[client]
base_url = "https://api.openai.com/v1"
timeout_secs = 60
# api_key = "set me or use OPENAI_API_KEY"

[agent]
model = "gpt-4"
max_iterations = 20
tool_timeout_secs = 30
request_timeout_secs = 120
stream = true
show_reasoning = true

[tui]
rich_output = true
landing_title = "HERMES"
prompt_placeholder = "Ask anything... \"Fix a TODO in the codebase\""
```

Or use environment variables:

```bash
export OPENAI_API_KEY=your_api_key_here
export OPENAI_BASE_URL=https://api.openai.com/v1
export HERMES_MODEL=gpt-4
```

See [hermes.example.toml](hermes.example.toml) for the full schema, including MCP, Skills, gateway, and tool/runtime defaults.

## TUI Overview

- `hermes chat` starts on a prompt-first landing screen
- `i` enters prompt editing, and typing on landing also bootstraps prompt entry immediately
- `Enter` runs the current prompt
- `Up` / `Down` in prompt mode replay recent prompts from history
- `Tab` cycles workspace panels
- `Up` / `Down` scroll the chat in command mode
- `PageUp`, `PageDown`, `Home`, and `End` scroll the conversation even while prompt mode is active
- `Ctrl+L` starts a fresh session when you want to discard the current conversation history
- After a run completes or fails, the workspace returns to prompt mode so you can send a follow-up in the same session
- `stream = false` now uses the non-streaming response path instead of the streaming parser

## Library Usage

```rust
use hermes_core::{
    agent::{HermesAgent, AgentConfig},
    client::{OpenAIClient, ClientConfig},
    tools::{HermesTool, ToolRegistry, ToolContext},
    schema::ToolSchema,
};
use async_trait::async_trait;
use serde_json::Value;

// Define a custom tool
struct MyTool;

#[async_trait]
impl HermesTool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "My custom tool" }
    fn schema(&self) -> ToolSchema { /* ... */ }

    async fn execute(&self, args: Value, context: ToolContext) -> ToolResult {
        // Your tool logic here
    }
}

// Create the agent
let client = OpenAIClient::new(ClientConfig::default());
let registry = ToolRegistry::new(std::time::Duration::from_secs(30));
registry.register(MyTool).await.unwrap();

let agent = HermesAgent::new(
    AgentConfig::default(),
    client,
    registry,
);

// Run the agent
let response = agent.run("Hello!").await?;
println!("{}", response.content);
```

## CLI Options

```
hermes [OPTIONS] <COMMAND>

Commands:
  run     Run the agent with a query
  tools   List available tools
  chat    Interactive chat mode
  test    Test a specific tool
  help    Print this message or the help of the given subcommand(s)

Options:
  -v, --verbose           Enable verbose output
  -l, --log-level <LOG>  Log level (debug, info, warn, error) [default: info]
  -c, --config <FILE>    Configuration file path
  --api-key <KEY>        OpenAI API key
  --base-url <URL>       OpenAI base URL
  -m, --model <MODEL>    Model to use [default: gpt-4]
  -i, --max-iterations <N>  Maximum iterations [default: 20]
  --tool-timeout <SECS>  Tool timeout in seconds [default: 30]
  --request-timeout <SECS>  Request timeout in seconds
  --context-window <TOKENS> Context window size
  --max-healing-attempts <N> Maximum self-healing retries
  --stream / --no-stream  Force streaming on or off
```

## Tool Definition

Tools are defined via the `HermesTool` trait. The framework automatically generates JSON Schema from your Rust structs:

```rust
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WeatherArgs {
    city: String,
    country: Option<String>,
}

struct WeatherTool;

#[async_trait]
impl HermesTool for WeatherTool {
    fn name(&self) -> &str { "get_weather" }
    fn description(&self) -> &str { "Get weather information for a city" }
    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<WeatherArgs>("get_weather", "Get weather information")
    }

    async fn execute(&self, args: Value, context: ToolContext) -> ToolResult {
        // Parse and execute
    }
}
```

## Error Handling

The library provides structured error types with self-healing capabilities:

```rust
use hermes_core::error::Error;

match result {
    Ok(response) => { /* handle success */ }
    Err(Error::ToolNotFound { name }) => {
        // Tool doesn't exist - self-healing will re-prompt LLM
    }
    Err(Error::ToolTimeout { name, timeout }) => {
        // Tool timed out - retry logic available
    }
    Err(e) => {
        // Other errors
    }
}
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for coding conventions, testing requirements, and the PR process.

Documentation and release hygiene for maintainers:

- keep `hermes.example.toml` in sync with runtime config changes
- add every user-facing change to `CHANGELOG.md` before cutting a tag
- update README screenshots or keybinding docs when TUI behavior changes
- update `AGENTS.md` / `CLAUDE.md` when the project context changes enough that an agent would otherwise rediscover it from scratch

- [Security Policy](SECURITY.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Changelog](CHANGELOG.md)

## Credits & Attribution

This project is a Rust implementation of the [Hermes-Agent](https://github.com/nousresearch/hermes-agent) originally developed by [Nous Research](https://nousresearch.com). 

While this is a "pure Rust" rewrite, the orchestration logic, system prompts, and architecture are based on the original work. This project is an unofficial community port and is not affiliated with or endorsed by Nous Research.
