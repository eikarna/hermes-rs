//! Terminal/shell command execution tool
//!
//! Provides secure shell command execution capabilities.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::runtime_config;
use crate::schema::ToolSchema;
use crate::tools::{KeruxTool, ToolContext, ToolResult};

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
    use_shell: Option<bool>,
}

/// Time still left before the command-level `deadline` is reached.
///
/// Every phase of command execution (stdout read, stderr read, final wait)
/// draws from ONE shared deadline instead of getting a fresh timeout window.
/// Returns `Duration::ZERO` once expired so `tokio::time::timeout` elapses
/// immediately rather than re-arming the full cap for the next phase.
fn remaining_budget(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .unwrap_or_default()
}

#[async_trait]
impl KeruxTool for TerminalTool {
    fn name(&self) -> &str {
        "terminal"
    }

    fn description(&self) -> &str {
        "Execute a command and return its output. Supports custom working directory and environment variables. Uses direct execution by default (preventing injection), but can use a shell if `useShell` is set to true."
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

        let mut cmd = {
            if args.use_shell.unwrap_or(false) {
                let shell = crate::platform::detect_shell();
                let mut c = Command::new(&shell.path);
                for arg in &shell.args_pattern {
                    c.arg(arg);
                }
                c.arg(&args.command);
                c
            } else {
                let parts = match shell_words::split(&args.command) {
                    Ok(p) => p,
                    Err(e) => return ToolResult::error("terminal", format!("Failed to parse command string: {}. Consider using useShell=true if you have special shell characters.", e)),
                };
                if parts.is_empty() {
                    return ToolResult::error("terminal", "Empty command string");
                }
                let mut c = Command::new(&parts[0]);
                c.args(&parts[1..]);
                c
            }
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

        // Single wall-clock deadline for the WHOLE command: stdout read,
        // stderr read, and final wait all draw from the same budget. The
        // previous code re-armed the full `timeout` per phase, so a process
        // that kept emitting output could run for unbounded wall time.
        let deadline = Instant::now() + timeout;

        cmd.kill_on_drop(true);

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
            while let Ok(Ok(Some(l))) =
                tokio::time::timeout(remaining_budget(deadline), reader.next_line()).await
            {
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
            while let Ok(Ok(Some(l))) =
                tokio::time::timeout(remaining_budget(deadline), reader.next_line()).await
            {
                if stderr_output.len() + l.len() < max_output / 4 {
                    stderr_output.push_str(&l);
                    stderr_output.push('\n');
                }
            }
        }

        // Wait for process to complete
        let status = match tokio::time::timeout(remaining_budget(deadline), child.wait()).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_terminal_tool_direct_execution() {
        let tool = TerminalTool;
        let command = if cfg!(windows) {
            "cmd /C echo hello world"
        } else {
            "printf 'hello world'"
        };
        let args = json!({
            "command": command
        });

        let result = tool.execute(args, ToolContext::default()).await;

        assert!(result.success);
    }

    #[test]
    fn test_remaining_budget_counts_down_to_zero() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let first = remaining_budget(deadline);
        assert!(first > Duration::ZERO);
        assert!(first <= Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(80));
        // Expired deadlines must yield ZERO, not panic or go negative,
        // so later phases fail immediately instead of re-arming the cap.
        assert_eq!(remaining_budget(deadline), Duration::ZERO);
    }

    /// Regression test for audit finding F1: the timeout window used to be
    /// re-armed for every `next_line()`/`wait()`, so a process emitting one
    /// line every 500ms kept the tool alive for unbounded wall time. The
    /// whole command must now be killed near the single shared deadline.
    ///
    /// NOTE: against the pre-fix code this test never completes (the read
    /// loops spin forever), which is exactly the reported bug.
    #[tokio::test]
    async fn test_timeout_bounds_endlessly_chatty_process() {
        let tool = TerminalTool;
        let command = if cfg!(windows) {
            "powershell.exe -NoProfile -Command \"while ($true) { Write-Output tick; Start-Sleep -Milliseconds 500 }\""
        } else {
            "sh -c 'while true; do echo tick; sleep 0.5; done'"
        };
        let args = json!({
            "command": command,
            "timeout": 2,
        });

        let started = Instant::now();
        let result = tool.execute(args, ToolContext::default()).await;
        let elapsed = started.elapsed();

        // The command must end via the shared-deadline timeout path...
        assert!(
            !result.success,
            "chatty process finished successfully instead of being killed: {:?}",
            result
        );
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("timed out"),
            "expected timeout error, got: {}",
            err
        );
        // ...after genuinely running for a while (the loop emits ~4 lines
        // before a 2s deadline), not dying instantly on spawn failure.
        assert!(
            elapsed >= Duration::from_millis(1500),
            "killed too early after {:?}; process may never have started",
            elapsed
        );
        // ...and near the configured cap, NOT running unbounded.
        assert!(
            elapsed < Duration::from_secs(6),
            "tool ran {:?} for a 2s cap — timeout still unbounded?",
            elapsed
        );
    }
}
