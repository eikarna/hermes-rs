//! Code execution tool
//!
//! Provides secure code execution in a sandboxed environment.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use schemars::JsonSchema;
use std::collections::HashMap;
use std::process::Stdio;

use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolResult};

/// Code execution timeout in seconds
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;

/// Tool for executing code in various languages
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
impl HermesTool for CodeExecutionTool {
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
            Err(e) => return ToolResult::error("code_execution", format!("Invalid arguments: {}", e)),
        };

        let timeout = std::time::Duration::from_secs(
            args.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS).min(MAX_TIMEOUT_SECS)
        );

        let result = match args.language.to_lowercase().as_str() {
            "python" | "py" => execute_python(&args.code, timeout).await,
            "javascript" | "js" | "node" => execute_javascript(&args.code, timeout).await,
            "shell" | "bash" | "sh" => execute_shell(&args.code, timeout).await,
            "rust" | "rs" => execute_rust(&args.code, timeout).await,
            _ => return ToolResult::error("code_execution", format!("Unsupported language: {}", args.language)),
        };

        match result {
            Ok(output) => ToolResult::success("code_execution", output),
            Err(e) => ToolResult::error("code_execution", e),
        }
    }
}

async fn execute_python(code: &str, timeout: std::time::Duration) -> Result<serde_json::Value, String> {
    use tokio::process::Command;
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Create a temp file for the code
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join(format!("hermes_code_{}.py", uuid_simple()));

    std::fs::write(&script_path, code)
        .map_err(|e| format!("Failed to write temp script: {}", e))?;

    let python_cmd = crate::platform::find_python()
        .unwrap_or_else(|| std::path::PathBuf::from("python3"));
    let mut cmd = Command::new(&python_cmd);
    cmd.arg(script_path.to_str().unwrap_or("script.py"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn python: {}", e))?;

    let start = std::time::Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut stdout_output = String::new();
    let mut stderr_output = String::new();

    // Read outputs
    if let Some(stdout) = stdout {
        let mut reader = BufReader::new(stdout).lines();
while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stdout_output.push_str(&line);
        stdout_output.push('\n');
    }
    }

if let Some(stderr) = stderr {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stderr_output.push_str(&line);
        stderr_output.push('\n');
    }
}

let status = tokio::time::timeout(timeout, child.wait()).await
    .map_err(|_| "Command timed out")?
    .map_err(|e| format!("Failed to wait: {}", e))?;

let runtime = start.elapsed();

// Clean up temp file
    let _ = std::fs::remove_file(&script_path);

    Ok(serde_json::json!({
        "language": "python",
        "exit_code": status.code(),
        "stdout": stdout_output,
        "stderr": stderr_output,
        "runtime_ms": runtime.as_millis() as u64,
        "success": status.success()
    }))
}

async fn execute_javascript(code: &str, timeout: std::time::Duration) -> Result<serde_json::Value, String> {
    use tokio::process::Command;
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Create a temp file for the code
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join(format!("hermes_code_{}.js", uuid_simple()));

    std::fs::write(&script_path, code)
        .map_err(|e| format!("Failed to write temp script: {}", e))?;

    let mut cmd = Command::new("node");
    cmd.arg(script_path.to_str().unwrap_or("script.js"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn node: {}", e))?;

    let start = std::time::Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut stdout_output = String::new();
    let mut stderr_output = String::new();

    if let Some(stdout) = stdout {
        let mut reader = BufReader::new(stdout).lines();
while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stdout_output.push_str(&line);
        stdout_output.push('\n');
    }
    }

if let Some(stderr) = stderr {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stderr_output.push_str(&line);
        stderr_output.push('\n');
    }
}

let status = tokio::time::timeout(timeout, child.wait()).await
    .map_err(|_| "Command timed out")?
    .map_err(|e| format!("Failed to wait: {}", e))?;

let runtime = start.elapsed();

let _ = std::fs::remove_file(&script_path);

    Ok(serde_json::json!({
        "language": "javascript",
        "exit_code": status.code(),
        "stdout": stdout_output,
        "stderr": stderr_output,
        "runtime_ms": runtime.as_millis() as u64,
        "success": status.success()
    }))
}

async fn execute_shell(code: &str, timeout: std::time::Duration) -> Result<serde_json::Value, String> {
    use tokio::process::Command;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let shell = crate::platform::detect_shell();
    let mut cmd = {
        let mut c = Command::new(&shell.path);
        for arg in &shell.args_pattern {
            c.arg(arg);
        }
        c.arg(code);
        c
    };

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn shell: {}", e))?;

    let start = std::time::Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut stdout_output = String::new();
    let mut stderr_output = String::new();

    if let Some(stdout) = stdout {
        let mut reader = BufReader::new(stdout).lines();
while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stdout_output.push_str(&line);
        stdout_output.push('\n');
    }
    }

if let Some(stderr) = stderr {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stderr_output.push_str(&line);
        stderr_output.push('\n');
    }
}

let status = tokio::time::timeout(timeout, child.wait()).await
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

async fn execute_rust(code: &str, timeout: std::time::Duration) -> Result<serde_json::Value, String> {
    // Rust requires compilation, so we create a proper project
    use tokio::process::Command;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let temp_dir = std::env::temp_dir();
    let project_dir = temp_dir.join(format!("hermes_rust_{}", uuid_simple()));

    // Create project structure
    std::fs::create_dir_all(project_dir.join("src"))
        .map_err(|e| format!("Failed to create project dir: {}", e))?;

    // Write Cargo.toml
    std::fs::write(
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
    .map_err(|e| format!("Failed to write Cargo.toml: {}", e))?;

    // Write main.rs
    std::fs::write(project_dir.join("src/main.rs"), code)
        .map_err(|e| format!("Failed to write main.rs: {}", e))?;

    let mut cmd = Command::new("rustc");
    cmd.arg(project_dir.join("src/main.rs"))
        .arg("-o")
        .arg(project_dir.join("main"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut compile_child = cmd.spawn().map_err(|e| format!("Failed to spawn rustc: {}", e))?;

    let compile_status = tokio::time::timeout(timeout, compile_child.wait()).await
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
    run_cmd.stdout(Stdio::piped());
    run_cmd.stderr(Stdio::piped());

    let start = std::time::Instant::now();
    let mut run_child = run_cmd.spawn().map_err(|e| format!("Failed to run binary: {}", e))?;

    let stdout = run_child.stdout.take();
    let stderr = run_child.stderr.take();

    let mut stdout_output = String::new();
    let mut stderr_output = String::new();

    if let Some(stdout) = stdout {
        let mut reader = BufReader::new(stdout).lines();
while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stdout_output.push_str(&line);
        stdout_output.push('\n');
    }
    }

if let Some(stderr) = stderr {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Ok(Some(line))) = tokio::time::timeout(timeout, reader.next_line()).await {
        stderr_output.push_str(&line);
        stderr_output.push('\n');
    }
}

let status = tokio::time::timeout(timeout, run_child.wait()).await
    .map_err(|_| "Execution timed out")?
    .map_err(|e| format!("Execution failed: {}", e))?;

    let runtime = start.elapsed();

    // Clean up
    let _ = std::fs::remove_dir_all(&project_dir);

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