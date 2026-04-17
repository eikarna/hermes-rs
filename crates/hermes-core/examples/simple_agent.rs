//! Simple Hermes-RS example
//!
//! This example demonstrates basic usage of the hermes-core library.

use std::time::Duration;

use async_trait::async_trait;
use hermes_core::{
    agent::{AgentConfig, HermesAgent},
    client::{ClientConfig, OpenAIClient},
    error::Result,
    schema::ToolSchema,
    tools::{HermesTool, ToolContext, ToolRegistry, ToolResult},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tracing_subscriber::FmtSubscriber;

/// Echo tool - demonstrates basic tool implementation
struct EchoTool;

#[async_trait]
impl HermesTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes back the input message. Useful for testing."
    }

    fn schema(&self) -> ToolSchema {
        #[derive(JsonSchema, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct EchoArgs {
            message: String,
        }
        ToolSchema::from_type::<EchoArgs>("echo", "Echoes back the input message")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("No message provided");

        ToolResult::success("echo", serde_json::json!({ "echoed": message }))
    }
}

/// Calculator tool - demonstrates more complex tool
struct CalculatorTool;

#[async_trait]
impl HermesTool for CalculatorTool {
    fn name(&self) -> &str {
        "calculate"
    }

    fn description(&self) -> &str {
        "Perform basic arithmetic operations: add, subtract, multiply, divide"
    }

    fn schema(&self) -> ToolSchema {
        #[derive(JsonSchema, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct CalcArgs {
            operation: String,
            a: f64,
            b: f64,
        }
        ToolSchema::from_type::<CalcArgs>("calculate", "Perform calculations")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let op = args
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("+");
        let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let result = match op {
            "+" | "add" => a + b,
            "-" | "subtract" => a - b,
            "*" | "multiply" => a * b,
            "/" | "divide" => {
                if b == 0.0 {
                    return ToolResult::error("calculate", "Division by zero");
                }
                a / b
            }
            _ => {
                return ToolResult::error("calculate", format!("Unknown operation: {}", op));
            }
        };

        ToolResult::success(
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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(true)
        .with_thread_ids(true)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    println!("Hermes-RS Simple Agent Example\n");
    println!("This example demonstrates how to use the hermes-core library.\n");

    // Build the client
    let config = ClientConfig {
        base_url: std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        api_key: std::env::var("OPENAI_API_KEY").ok(),
        timeout: Duration::from_secs(60),
        max_context_length: 128_000,
    };

    let client = OpenAIClient::new(config);
    println!("Client configured.\n");

    // Build the tool registry
    let registry = ToolRegistry::new(Duration::from_secs(30));
    registry.register(EchoTool).await?;
    registry.register(CalculatorTool).await?;

    let tools = registry.get_schemas().await;
    println!("Registered {} tools:", tools.len());
    for tool in &tools {
        println!(" - {}: {}", tool.name, tool.description);
    }
    println!();

    // Build the agent
    let agent_config = AgentConfig {
        model: std::env::var("MODEL").unwrap_or_else(|_| "gpt-4".to_string()),
        max_iterations: 10,
        tool_timeout: Duration::from_secs(30),
        system_prompt: Some(
            "You are a helpful assistant with access to echo and calculate tools. \
Use the echo tool to repeat information and the calculate tool for math."
                .to_string(),
        ),
        stream: true,
        context_window: 128_000,
        request_timeout: Duration::from_secs(120),
    };

    let agent = HermesAgent::new(agent_config, client, registry);

    // Run a query
    let query = "Please use the calculate tool to compute 15 + 27, then echo the result.";
    println!("Query: {}\n", query);
    println!("Running agent...\n");

    match agent.run(query.to_string()).await {
        Ok(response) => {
            println!("\n=== Agent Response ===");
            println!("{}", response.content);
            println!("\n=== End ===");
        }
        Err(e) => {
            eprintln!("Agent error: {}", e);
            eprintln!("\nNote: This example requires a valid OpenAI API key.");
            eprintln!("Set it with: export OPENAI_API_KEY=your_key_here");
        }
    }

    Ok(())
}
