//! Code execution tool
//!
//! Launches code as child processes on the host. Configured timeouts bound
//! individual output reads and waits; they do not currently guarantee that a
//! timed-out child is terminated. This tool does not isolate the filesystem,
//! network, environment, or process privileges. Use an external sandbox such
//! as a container or VM when executing untrusted code.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;

use crate::config::runtime_config;
use crate::schema::ToolSchema;
use crate::tools::{KeruxTool, ToolContext, ToolResult};

/// Tool for executing code in various languages as host child processes.
pub struct CodeExecutionTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeExecutionArgs {
    code: String,
    language: String,
    /// Additional command-line arguments to pass to the script
    #[allow(dead_code)]
    args: Option<Vec<String>>,
    /// Environment variables to set for the execution
    #[allow(dead_code)]
    env_vars: Option<HashMap<String, String>>,
    timeout: Option<u64>,
}

#[async_trait]
impl KeruxTool for CodeExecutionTool {
    fn name(&self) -> &str {
        "code_execution"
    }

    fn description(&self) -> &str {
        "Execute code in various programming languages (python, javascript, rust, shell). \
        Returns stdout, stderr, and execution time."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<CodeExecutionArgs>("code_execution", "Execute code")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: CodeExecutionArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolResult::error("code_execution", format!("Invalid arguments: {}", e))
            }
        };
        let settings = runtime_config().tools.code_execution;

        let timeout = std::time::Duration::from_secs(
            args.timeout
                .unwrap_or(settings.default_timeout_secs)
                .min(settings.max_timeout_secs),
        );

        let result = match args.language.to_lowercase().as_str() {
            "python" | "py" => execute_python(&args.code, timeout).await,
            "javascript" | "js" | "node" => execute_javascript(&args.code, timeout).await,
            "shell" | "bash" | "sh" => execute_shell(&args.code, timeout).await,
            "rust" | "rs" => execute_rust(&args.code, timeout).await,
            _ => {
                return ToolResult::error(
                    "code_execution",
                    format!("Unsupported language: {}", args.language),
                )
            }
        };

        match result {
            Ok(output) => ToolResult::success("code_execution", output),
            Err(e) => ToolResult::error("code_execution", e),
        }
    }
}

async fn execute_python(
    code: &str,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    // Create a temp file for the code
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join(format!("kerux_code_{}.py", uuid_simple()));

    tokio::fs::write(&script_path, code)
        .await
        .map_err(|e| format!("Failed to write temp script: {}", e))?;

    let python_cmd =
        crate::platform::find_python().unwrap_or_else(|| std::path::PathBuf::from("python3"));
    let mut cmd = Command::new(&python_cmd);
    cmd.kill_on_drop(true);
    cmd.arg(script_path.to_str().unwrap_or("script.py"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn python: {}", e))?;

    let start = std::time::Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Read stdout/stderr concurrently — sequential reads deadlock when the child
    // fills one pipe's OS buffer while we block reading the other.
    let out_fut = async move {
        let mut buf = String::new();
        if let Some(stdout) = stdout {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let err_fut = async move {
        let mut buf = String::new();
        if let Some(stderr) = stderr {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let (stdout_output, stderr_output) = tokio::join!(out_fut, err_fut);

    let status = tokio::time::timeout(timeout, child.wait())
        .await
        .map_err(|_| "Command timed out")?
        .map_err(|e| format!("Failed to wait: {}", e))?;

    let runtime = start.elapsed();

    // Clean up temp file
    let _ = tokio::fs::remove_file(&script_path).await;

    Ok(serde_json::json!({
        "language": "python",
        "exit_code": status.code(),
        "stdout": stdout_output,
        "stderr": stderr_output,
        "runtime_ms": runtime.as_millis() as u64,
        "success": status.success()
    }))
}

async fn execute_javascript(
    code: &str,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    // Create a temp file for the code
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join(format!("kerux_code_{}.js", uuid_simple()));

    tokio::fs::write(&script_path, code)
        .await
        .map_err(|e| format!("Failed to write temp script: {}", e))?;

    let mut cmd = Command::new("node");
    cmd.kill_on_drop(true);
    cmd.arg(script_path.to_str().unwrap_or("script.js"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn node: {}", e))?;

    let start = std::time::Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Read stdout/stderr concurrently — sequential reads deadlock when the child
    // fills one pipe's OS buffer while we block reading the other.
    let out_fut = async move {
        let mut buf = String::new();
        if let Some(stdout) = stdout {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let err_fut = async move {
        let mut buf = String::new();
        if let Some(stderr) = stderr {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let (stdout_output, stderr_output) = tokio::join!(out_fut, err_fut);

    let status = tokio::time::timeout(timeout, child.wait())
        .await
        .map_err(|_| "Command timed out")?
        .map_err(|e| format!("Failed to wait: {}", e))?;

    let runtime = start.elapsed();

    let _ = tokio::fs::remove_file(&script_path).await;

    Ok(serde_json::json!({
        "language": "javascript",
        "exit_code": status.code(),
        "stdout": stdout_output,
        "stderr": stderr_output,
        "runtime_ms": runtime.as_millis() as u64,
        "success": status.success()
    }))
}

async fn execute_shell(
    code: &str,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let shell = crate::platform::detect_shell();
    let mut cmd = {
        let mut c = Command::new(&shell.path);
        c.kill_on_drop(true);
        for arg in &shell.args_pattern {
            c.arg(arg);
        }
        c.arg(code);
        c
    };

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn shell: {}", e))?;

    let start = std::time::Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Read stdout/stderr concurrently — sequential reads deadlock when the child
    // fills one pipe's OS buffer while we block reading the other.
    let out_fut = async move {
        let mut buf = String::new();
        if let Some(stdout) = stdout {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let err_fut = async move {
        let mut buf = String::new();
        if let Some(stderr) = stderr {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let (stdout_output, stderr_output) = tokio::join!(out_fut, err_fut);

    let status = tokio::time::timeout(timeout, child.wait())
        .await
        .map_err(|_| "Command timed out")?
        .map_err(|e| format!("Failed to wait: {}", e))?;

    let runtime = start.elapsed();

    Ok(serde_json::json!({
    "language": "shell",
        "exit_code": status.code(),
        "stdout": stdout_output,
        "stderr": stderr_output,
        "runtime_ms": runtime.as_millis() as u64,
        "success": status.success()
    }))
}

async fn execute_rust(
    code: &str,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    // Rust requires compilation, so we create a proper project
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let temp_dir = std::env::temp_dir();
    let project_dir = temp_dir.join(format!("kerux_rust_{}", uuid_simple()));

    // Create project structure
    tokio::fs::create_dir_all(project_dir.join("src"))
        .await
        .map_err(|e| format!("Failed to create project dir: {}", e))?;

    // Write Cargo.toml
    tokio::fs::write(
        project_dir.join("Cargo.toml"),
        r#"[package]
name = "temp"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "main"
path = "src/main.rs"
"#,
    )
    .await
    .map_err(|e| format!("Failed to write Cargo.toml: {}", e))?;

    // Write main.rs
    tokio::fs::write(project_dir.join("src/main.rs"), code)
        .await
        .map_err(|e| format!("Failed to write main.rs: {}", e))?;

    let mut cmd = Command::new("rustc");
    cmd.kill_on_drop(true);
    cmd.arg(project_dir.join("src/main.rs"))
        .arg("-o")
        .arg(project_dir.join("main"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut compile_child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn rustc: {}", e))?;

    let compile_status = tokio::time::timeout(timeout, compile_child.wait())
        .await
        .map_err(|_| "Compilation timed out")?
        .map_err(|e| format!("Compilation failed: {}", e))?;

    if !compile_status.success() {
        let mut stderr_output = String::new();
        if let Some(stderr) = compile_child.stderr.take() {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                stderr_output.push_str(&line);
                stderr_output.push('\n');
            }
        }
        return Ok(serde_json::json!({
            "language": "rust",
            "exit_code": compile_status.code(),
            "stdout": "",
            "stderr": stderr_output,
            "runtime_ms": 0,
            "success": false,
            "stage": "compilation"
        }));
    }

    // Run the compiled binary
    let mut run_cmd = Command::new(project_dir.join("main").to_str().unwrap_or("main"));
    run_cmd.kill_on_drop(true);
    run_cmd.stdout(Stdio::piped());
    run_cmd.stderr(Stdio::piped());

    let start = std::time::Instant::now();
    let mut run_child = run_cmd
        .spawn()
        .map_err(|e| format!("Failed to run binary: {}", e))?;

    let stdout = run_child.stdout.take();
    let stderr = run_child.stderr.take();

    // Read stdout/stderr concurrently — sequential reads deadlock when the child
    // fills one pipe's OS buffer while we block reading the other.
    let out_fut = async move {
        let mut buf = String::new();
        if let Some(stdout) = stdout {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let err_fut = async move {
        let mut buf = String::new();
        if let Some(stderr) = stderr {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    };
    let (stdout_output, stderr_output) = tokio::join!(out_fut, err_fut);

    let status = tokio::time::timeout(timeout, run_child.wait())
        .await
        .map_err(|_| "Execution timed out")?
        .map_err(|e| format!("Execution failed: {}", e))?;

    let runtime = start.elapsed();

    // Clean up
    let _ = tokio::fs::remove_dir_all(&project_dir).await;

    Ok(serde_json::json!({
        "language": "rust",
        "exit_code": status.code(),
        "stdout": stdout_output,
        "stderr": stderr_output,
        "runtime_ms": runtime.as_millis() as u64,
        "success": status.success()
    }))
}

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}{:x}", now.as_secs(), now.subsec_nanos())
}
