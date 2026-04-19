//! Terminal/shell command execution tool
//!
//! Provides secure shell command execution capabilities.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::runtime_config;
use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolResult};

/// Tool for executing shell commands
pub struct TerminalTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalArgs {
    command: String,
    working_dir: Option<String>,
    env_vars: Option<HashMap<String, String>>,
    timeout: Option<u64>,
    max_output: Option<usize>,
}

#[async_trait]
impl HermesTool for TerminalTool {
    fn name(&self) -> &str {
        "terminal"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output. Supports custom working directory and environment variables."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<TerminalArgs>("terminal", "Execute shell command")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: TerminalArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("terminal", format!("Invalid arguments: {}", e)),
        };
        let settings = runtime_config().tools.terminal;

        let timeout = std::time::Duration::from_secs(
            args.timeout
                .unwrap_or(settings.max_timeout_secs)
                .min(settings.max_timeout_secs),
        );
        let max_output = args.max_output.unwrap_or(settings.max_output_bytes);

        let shell = crate::platform::detect_shell();
        let mut cmd = {
            let mut c = Command::new(&shell.path);
            for arg in &shell.args_pattern {
                c.arg(arg);
            }
            c.arg(&args.command);
            c
        };

        // Set working directory
        if let Some(ref dir) = args.working_dir {
            cmd.current_dir(dir);
        } else {
            // Use current directory as default
            if let Ok(cwd) = std::env::current_dir() {
                cmd.current_dir(cwd);
            }
        }

        // Set environment variables
        if let Some(ref env_vars) = args.env_vars {
            // Start with current environment
            let mut env = std::env::vars().collect::<HashMap<_, _>>();
            // Add/override with provided variables
            for (key, value) in env_vars {
                env.insert(key.clone(), value.clone());
            }
            // Pass to command
            cmd.envs(&env);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error("terminal", format!("Failed to spawn process: {}", e))
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let mut stdout_output = String::new();
        let mut stderr_output = String::new();

        // Read stdout
        if let Some(stdout) = stdout {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Ok(Some(l))) = tokio::time::timeout(timeout, reader.next_line()).await {
                if stdout_output.len() + l.len() < max_output {
                    stdout_output.push_str(&l);
                    stdout_output.push('\n');
                } else if stdout_output.len() < max_output {
                    let remaining = max_output - stdout_output.len();
                    stdout_output.push_str(&l[..remaining.min(l.len())]);
                    stdout_output.push_str("\n[output truncated]");
                } else {
                    stdout_output.push_str("\n[output truncated]");
                    break;
                }
            }
        }

        // Read stderr
        if let Some(stderr) = stderr {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Ok(Some(l))) = tokio::time::timeout(timeout, reader.next_line()).await {
                if stderr_output.len() + l.len() < max_output / 4 {
                    stderr_output.push_str(&l);
                    stderr_output.push('\n');
                }
            }
        }

        // Wait for process to complete
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return ToolResult::error("terminal", format!("Failed to wait for process: {}", e))
            }
            Err(_) => {
                let _ = child.kill().await;
                return ToolResult::error(
                    "terminal",
                    format!("Command timed out after {:?}", timeout),
                );
            }
        };

        let exit_code = status.code();

        if status.success() {
            ToolResult::success(
                "terminal",
                serde_json::json!({
                    "success": true,
                    "exit_code": exit_code,
                    "stdout": stdout_output,
                    "stderr": stderr_output,
                    "runtime": "Command completed successfully"
                }),
            )
        } else {
            ToolResult::success(
                "terminal",
                serde_json::json!({
                    "success": false,
                    "exit_code": exit_code,
                    "stdout": stdout_output,
                    "stderr": stderr_output,
                    "runtime": "Command failed"
                }),
            )
        }
    }
}
