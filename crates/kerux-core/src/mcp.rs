//! MCP (Model Context Protocol) client for Kerux
//!
//! Provides integration with MCP servers to extend the agent's capabilities
//! with tools and resources from external sources.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::error::Result;
use crate::schema::ToolSchema;
use crate::tools::{KeruxTool, ToolContext, ToolResult};

/// MCP protocol version
const MCP_VERSION: &str = "2024-11-05";

/// MCP client for connecting to MCP servers
#[derive(Debug, Clone)]
pub struct McpClient {
    /// Server URL
    url: String,
    /// Authentication token
    auth_token: Option<String>,
    /// HTTP client
    client: reqwest::Client,
    /// Connected tools from this server
    tools: Arc<RwLock<Arc<Vec<McpTool>>>>,
    /// Server capabilities
    capabilities: Arc<RwLock<McpCapabilities>>,
    /// Whether connected
    connected: Arc<RwLock<bool>>,
}

/// Server capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpCapabilities {
    /// Supports tools
    pub tools: bool,
    /// Supports resources
    pub resources: bool,
    /// Supports prompts
    pub prompts: bool,
}

/// Initialize request
#[derive(Debug, Serialize)]
struct InitializeRequest {
    protocol_version: String,
    capabilities: ClientCapabilities,
    client_info: ClientInfo,
}

/// Client capabilities
#[derive(Debug, Serialize)]
struct ClientCapabilities {
    #[serde(rename = "roots")]
    roots: Option<Roots>,
    #[serde(rename = "sampling")]
    sampling: Option<Sampling>,
}

/// Roots capability
#[derive(Debug, Serialize)]
struct Roots {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

/// Sampling capability
#[derive(Debug, Serialize)]
struct Sampling {}

/// Client info
#[derive(Debug, Serialize)]
struct ClientInfo {
    name: String,
    version: String,
}

/// Initialize response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InitializeResponse {
    protocol_version: String,
    capabilities: ServerCapabilities,
    server_info: ServerInfo,
}

/// Server capabilities
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ServerCapabilities {
    #[serde(rename = "tools")]
    tools: Option<ToolsCapability>,
    #[serde(rename = "resources")]
    resources: Option<ResourcesCapability>,
    #[serde(rename = "prompts")]
    prompts: Option<PromptsCapability>,
}

/// Tools capability
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ToolsCapability {
    #[serde(rename = "listChanged")]
    list_changed: Option<bool>,
}

/// Resources capability
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResourcesCapability {
    #[serde(rename = "subscribe")]
    subscribe: Option<bool>,
    #[serde(rename = "listChanged")]
    list_changed: Option<bool>,
}

/// Prompts capability
#[derive(Debug, Deserialize)]
struct PromptsCapability {}

/// Server info
#[derive(Debug, Deserialize)]
struct ServerInfo {
    name: String,
    version: String,
}

/// JSON-RPC request
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Option<Value>,
    id: u64,
}

/// JSON-RPC response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: u64,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

/// JSON-RPC error
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcError {
    code: i32,
    message: String,
    data: Option<Value>,
}

/// Tool listing
#[derive(Debug, Deserialize)]
struct ToolListResult {
    tools: Vec<McpToolDefinition>,
}

/// Tool definition from MCP server
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpClient {
    /// Create a new MCP client
    pub fn new(url: impl Into<String>, auth_token: Option<String>) -> Self {
        Self {
            url: url.into(),
            auth_token,
            client: reqwest::Client::new(),
            tools: Arc::new(RwLock::new(Arc::new(Vec::new()))),
            capabilities: Arc::new(RwLock::new(McpCapabilities::default())),
            connected: Arc::new(RwLock::new(false)),
        }
    }

    /// Connect to the MCP server and initialize
    pub async fn connect(&self) -> Result<()> {
        info!(url = %self.url, "Connecting to MCP server");

        let request = InitializeRequest {
            protocol_version: MCP_VERSION.to_string(),
            capabilities: ClientCapabilities {
                roots: Some(Roots { list_changed: true }),
                sampling: Some(Sampling {}),
            },
            client_info: ClientInfo {
                name: "kerux".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let response = self
            .send_request("initialize", Some(serde_json::to_value(request)?))
            .await?;

        let init_response: InitializeResponse = serde_json::from_value(response).map_err(|e| {
            crate::error::Error::ParseResponse(format!(
                "Failed to parse initialize response: {}",
                e
            ))
        })?;

        debug!(
            server = %init_response.server_info.name,
            version = %init_response.server_info.version,
            "MCP server initialized"
        );

        // Update capabilities
        {
            let mut caps = self.capabilities.write().await;
            caps.tools = init_response.capabilities.tools.is_some();
            caps.resources = init_response.capabilities.resources.is_some();
            caps.prompts = init_response.capabilities.prompts.is_some();
        }

        // Send initialized notification
        self.send_notification("initialized", Value::Null).await?;

        // List available tools
        self.list_tools().await?;

        *self.connected.write().await = true;
        info!(url = %self.url, "Connected to MCP server");

        Ok(())
    }

    /// Disconnect from the MCP server
    pub async fn disconnect(&self) -> Result<()> {
        *self.connected.write().await = false;
        *self.tools.write().await = Arc::new(Vec::new());
        info!(url = %self.url, "Disconnected from MCP server");
        Ok(())
    }

    /// Check if connected
    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }

    /// List tools from the server
    pub async fn list_tools(&self) -> Result<Vec<McpToolDefinition>> {
        let response = self.send_request("tools/list", None).await?;
        let tool_list: ToolListResult = serde_json::from_value(response).map_err(|e| {
            crate::error::Error::ParseResponse(format!("Failed to parse tool list: {}", e))
        })?;

        let tools: Vec<McpTool> = tool_list
            .tools
            .into_iter()
            .map(|def| McpTool::new(self.clone(), def))
            .collect();

        *self.tools.write().await = Arc::new(tools);

        debug!(count = self.tools.read().await.len(), "Listed MCP tools");
        Ok(self
            .tools
            .read()
            .await
            .iter()
            .map(|t| (*t.definition).clone())
            .collect())
    }

    /// Call a tool on the MCP server
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });

        let response = self.send_request("tools/call", Some(params)).await?;
        Ok(response)
    }

    /// Get all tools
    pub async fn get_tools(&self) -> Arc<Vec<McpTool>> {
        self.tools.read().await.clone()
    }

    /// Get server capabilities
    pub async fn get_capabilities(&self) -> McpCapabilities {
        self.capabilities.read().await.clone()
    }

    /// Send a JSON-RPC request
    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let request_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: request_id,
        };

        let mut req_builder = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json");

        if let Some(ref token) = self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder.json(&request).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await?;
            error!(status = %status, body = %body, "MCP request failed");
            return Err(crate::error::Error::Agent(format!(
                "MCP request failed: {} - {}",
                status, body
            )));
        }

        let rpc_response: JsonRpcResponse = response.json().await?;

        if let Some(error) = rpc_response.error {
            return Err(crate::error::Error::Agent(format!(
                "MCP error {}: {}",
                error.code, error.message
            )));
        }

        rpc_response
            .result
            .ok_or_else(|| crate::error::Error::Agent("No result in MCP response".to_string()))
    }

    /// Send a notification (no response expected)
    async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(params),
            id: 0,
        };

        let mut req_builder = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json");

        if let Some(ref token) = self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let _ = req_builder.json(&request).send().await;
        Ok(())
    }
}

/// Bundled stdin/stdout for a stdio MCP transport
#[derive(Debug)]
struct StdioIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// MCP client that communicates over stdin/stdout of a child process
#[derive(Debug, Clone)]
pub struct McpStdioClient {
    /// Command to spawn
    command: String,
    /// Arguments for the command
    args: Vec<String>,
    /// Environment variables for the child process
    env: HashMap<String, String>,
    /// Child process handle
    child: Arc<RwLock<Option<Child>>>,
    /// Stdin/stdout IO pair (locked together for request-response atomicity)
    io: Arc<tokio::sync::Mutex<Option<StdioIo>>>,
    /// Connected tools from this server
    tools: Arc<RwLock<Arc<Vec<McpTool>>>>,
    /// Server capabilities
    capabilities: Arc<RwLock<McpCapabilities>>,
    /// Whether connected
    connected: Arc<RwLock<bool>>,
    /// Atomic request ID counter
    request_id: Arc<AtomicU64>,
}

impl McpStdioClient {
    /// Create a new stdio MCP client
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            env,
            child: Arc::new(RwLock::new(None)),
            io: Arc::new(tokio::sync::Mutex::new(None)),
            tools: Arc::new(RwLock::new(Arc::new(Vec::new()))),
            capabilities: Arc::new(RwLock::new(McpCapabilities::default())),
            connected: Arc::new(RwLock::new(false)),
            request_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Connect to the MCP server by spawning the child process and initializing
    pub async fn connect(&self) -> Result<()> {
        info!(command = %self.command, "Spawning MCP stdio server");

        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.kill_on_drop(true);
        cmd.args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            crate::error::Error::Agent(format!(
                "Failed to spawn MCP server '{}': {}",
                self.command, e
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            crate::error::Error::Agent("Failed to capture child stdin".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            crate::error::Error::Agent("Failed to capture child stdout".to_string())
        })?;

        *self.child.write().await = Some(child);
        *self.io.lock().await = Some(StdioIo {
            stdin,
            stdout: BufReader::new(stdout),
        });

        // Send initialize request
        let request = InitializeRequest {
            protocol_version: MCP_VERSION.to_string(),
            capabilities: ClientCapabilities {
                roots: Some(Roots { list_changed: true }),
                sampling: Some(Sampling {}),
            },
            client_info: ClientInfo {
                name: "kerux".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let response = self
            .send_request("initialize", Some(serde_json::to_value(request)?))
            .await?;

        let init_response: InitializeResponse = serde_json::from_value(response).map_err(|e| {
            crate::error::Error::ParseResponse(format!(
                "Failed to parse initialize response: {}",
                e
            ))
        })?;

        debug!(
            server = %init_response.server_info.name,
            version = %init_response.server_info.version,
            "MCP stdio server initialized"
        );

        // Update capabilities
        {
            let mut caps = self.capabilities.write().await;
            caps.tools = init_response.capabilities.tools.is_some();
            caps.resources = init_response.capabilities.resources.is_some();
            caps.prompts = init_response.capabilities.prompts.is_some();
        }

        // Send initialized notification
        self.send_notification("initialized", Value::Null).await?;

        // List available tools
        self.list_tools().await?;

        *self.connected.write().await = true;
        info!(command = %self.command, "Connected to MCP stdio server");

        Ok(())
    }

    /// Disconnect from the MCP server by killing the child process
    pub async fn disconnect(&self) -> Result<()> {
        *self.connected.write().await = false;
        *self.tools.write().await = Arc::new(Vec::new());

        // Drop IO handles to close stdin (signals EOF to child)
        *self.io.lock().await = None;

        // Kill child process if still running
        if let Some(mut child) = self.child.write().await.take() {
            if let Err(e) = child.kill().await {
                warn!(error = %e, "Failed to kill MCP stdio server process");
            } else {
                debug!("MCP stdio server process killed");
            }
        }

        info!(command = %self.command, "Disconnected from MCP stdio server");
        Ok(())
    }

    /// Check if connected
    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }

    /// List tools from the server
    pub async fn list_tools(&self) -> Result<Vec<McpToolDefinition>> {
        let response = self.send_request("tools/list", None).await?;
        let tool_list: ToolListResult = serde_json::from_value(response).map_err(|e| {
            crate::error::Error::ParseResponse(format!("Failed to parse tool list: {}", e))
        })?;

        let tools: Vec<McpTool> = tool_list
            .tools
            .into_iter()
            .map(|def| McpTool::new_stdio(self.clone(), def))
            .collect();

        *self.tools.write().await = Arc::new(tools);

        debug!(
            count = self.tools.read().await.len(),
            "Listed MCP stdio tools"
        );
        Ok(self
            .tools
            .read()
            .await
            .iter()
            .map(|t| (*t.definition).clone())
            .collect())
    }

    /// Call a tool on the MCP server
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });

        let response = self.send_request("tools/call", Some(params)).await?;
        Ok(response)
    }

    /// Get all tools
    pub async fn get_tools(&self) -> Arc<Vec<McpTool>> {
        self.tools.read().await.clone()
    }

    /// Get server capabilities
    pub async fn get_capabilities(&self) -> McpCapabilities {
        self.capabilities.read().await.clone()
    }

    /// Send a JSON-RPC request over stdin and read response from stdout
    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let request_id = self.request_id.fetch_add(1, Ordering::SeqCst);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: request_id,
        };

        let mut request_line = serde_json::to_string(&request).map_err(|e| {
            crate::error::Error::Agent(format!("Failed to serialize request: {}", e))
        })?;
        request_line.push('\n');

        let mut io_guard = self.io.lock().await;
        let io = io_guard.as_mut().ok_or_else(|| {
            crate::error::Error::Agent("MCP stdio transport not connected".to_string())
        })?;

        // Write request to stdin
        io.stdin
            .write_all(request_line.as_bytes())
            .await
            .map_err(|e| {
                crate::error::Error::Agent(format!("Failed to write to MCP stdin: {}", e))
            })?;
        io.stdin
            .flush()
            .await
            .map_err(|e| crate::error::Error::Agent(format!("Failed to flush MCP stdin: {}", e)))?;

        // Read response from stdout
        let mut response_line = String::new();
        io.stdout.read_line(&mut response_line).await.map_err(|e| {
            crate::error::Error::Agent(format!("Failed to read from MCP stdout: {}", e))
        })?;

        if response_line.is_empty() {
            return Err(crate::error::Error::Agent(
                "MCP server closed stdout unexpectedly".to_string(),
            ));
        }

        let rpc_response: JsonRpcResponse =
            serde_json::from_str(response_line.trim()).map_err(|e| {
                crate::error::Error::ParseResponse(format!(
                    "Failed to parse MCP stdio response: {}",
                    e
                ))
            })?;

        if let Some(error) = rpc_response.error {
            return Err(crate::error::Error::Agent(format!(
                "MCP error {}: {}",
                error.code, error.message
            )));
        }

        rpc_response
            .result
            .ok_or_else(|| crate::error::Error::Agent("No result in MCP response".to_string()))
    }

    /// Send a notification (no response expected)
    async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(params),
            id: 0,
        };

        let mut request_line = serde_json::to_string(&request).map_err(|e| {
            crate::error::Error::Agent(format!("Failed to serialize notification: {}", e))
        })?;
        request_line.push('\n');

        let mut io_guard = self.io.lock().await;
        if let Some(io) = io_guard.as_mut() {
            let _ = io.stdin.write_all(request_line.as_bytes()).await;
            let _ = io.stdin.flush().await;
        }

        Ok(())
    }
}

/// MCP transport type — either HTTP or stdio
#[derive(Debug, Clone)]
pub enum McpTransport {
    /// HTTP-based MCP client
    Http(McpClient),
    /// Stdio-based MCP client (child process)
    Stdio(McpStdioClient),
}

impl McpTransport {
    /// Check if the transport is connected
    pub async fn is_connected(&self) -> bool {
        match self {
            McpTransport::Http(c) => c.is_connected().await,
            McpTransport::Stdio(c) => c.is_connected().await,
        }
    }

    /// Get all tools from this transport
    pub async fn get_tools(&self) -> Arc<Vec<McpTool>> {
        match self {
            McpTransport::Http(c) => c.get_tools().await,
            McpTransport::Stdio(c) => c.get_tools().await,
        }
    }

    /// Disconnect the transport
    pub async fn disconnect(&self) -> Result<()> {
        match self {
            McpTransport::Http(c) => c.disconnect().await,
            McpTransport::Stdio(c) => c.disconnect().await,
        }
    }

    /// Call a tool on this transport
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        match self {
            McpTransport::Http(c) => c.call_tool(name, arguments).await,
            McpTransport::Stdio(c) => c.call_tool(name, arguments).await,
        }
    }
}

/// A tool from an MCP server
#[derive(Debug, Clone)]
pub struct McpTool {
    transport: McpTransport,
    definition: Arc<McpToolDefinition>,
}

impl McpTool {
    /// Create a new MCP tool wrapper (HTTP transport)
    pub fn new(client: McpClient, definition: McpToolDefinition) -> Self {
        Self {
            transport: McpTransport::Http(client),
            definition: Arc::new(definition),
        }
    }

    /// Create a new MCP tool wrapper (stdio transport)
    pub fn new_stdio(client: McpStdioClient, definition: McpToolDefinition) -> Self {
        Self {
            transport: McpTransport::Stdio(client),
            definition: Arc::new(definition),
        }
    }

    /// Get the tool name
    pub fn name(&self) -> &str {
        &self.definition.name
    }

    /// Get the tool definition
    pub fn definition(&self) -> &McpToolDefinition {
        &self.definition
    }
}

#[async_trait]
impl KeruxTool for McpTool {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn schema(&self) -> ToolSchema {
        let params = serde_json::to_value(&self.definition.input_schema)
            .unwrap_or_else(|_| serde_json::json!({"type": "object"}));

        ToolSchema::new(&self.definition.name, &self.definition.description, params)
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let name = self.definition.name.clone();

        match self.transport.call_tool(&name, args).await {
            Ok(result) => ToolResult::success(name, result),
            Err(e) => ToolResult::error(name, e.to_string()),
        }
    }
}

/// MCP server connection manager
#[derive(Debug, Default)]
pub struct McpManager {
    /// Connected servers (HTTP and stdio)
    servers: HashMap<String, McpTransport>,
}

/// Discovered MCP Server configuration from standard files (e.g. ~/.kerux/mcp.json or Claude Desktop config)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredMcpServer {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    pub auth_token: Option<String>,
    pub source: String,
}

/// JSON format used by Claude Desktop and standard MCP configs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RawMcpConfigFile {
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: HashMap<String, RawMcpServerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RawMcpServerEntry {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, rename = "authToken")]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub disabled: Option<bool>,
}

impl McpManager {
    /// Discovers MCP server configurations from default files:
    /// 1. ~/.kerux/mcp.json
    /// 2. Claude Desktop config (%APPDATA%\Claude\claude_desktop_config.json on Windows,
    ///    ~/Library/Application Support/Claude/claude_desktop_config.json on macOS,
    ///    ~/.config/Claude/claude_desktop_config.json on Linux)
    pub fn discover_configs() -> Vec<DiscoveredMcpServer> {
        let mut discovered = Vec::new();

        // 1. ~/.kerux/mcp.json
        if let Some(home) = dirs::home_dir() {
            let kerux_mcp_path = home.join(".kerux").join("mcp.json");
            if kerux_mcp_path.is_file() {
                discovered.extend(Self::parse_config_file(
                    &kerux_mcp_path,
                    "~/.kerux/mcp.json",
                ));
            }
        }

        // 2. Claude Desktop config
        for claude_path in Self::claude_desktop_config_paths() {
            if claude_path.is_file() {
                let source_label = claude_path.to_string_lossy().to_string();
                discovered.extend(Self::parse_config_file(&claude_path, &source_label));
                break;
            }
        }

        discovered
    }

    /// Candidate paths for Claude Desktop configuration across platforms
    pub fn claude_desktop_config_paths() -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();

        #[cfg(target_os = "windows")]
        {
            if let Some(app_data) = dirs::config_dir() {
                // dirs::config_dir() returns %APPDATA% on Windows (e.g. C:\Users\<User>\AppData\Roaming)
                paths.push(app_data.join("Claude").join("claude_desktop_config.json"));
            }
            if let Ok(appdata_env) = std::env::var("APPDATA") {
                let p = std::path::PathBuf::from(appdata_env)
                    .join("Claude")
                    .join("claude_desktop_config.json");
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            if let Some(home) = dirs::home_dir() {
                paths.push(
                    home.join("Library")
                        .join("Application Support")
                        .join("Claude")
                        .join("claude_desktop_config.json"),
                );
            }
        }

        #[cfg(target_os = "linux")]
        {
            if let Some(config_dir) = dirs::config_dir() {
                paths.push(config_dir.join("Claude").join("claude_desktop_config.json"));
            }
            if let Some(home) = dirs::home_dir() {
                paths.push(
                    home.join(".config")
                        .join("Claude")
                        .join("claude_desktop_config.json"),
                );
            }
        }

        // Fallback for general platforms or if OS-specific check missed
        if let Some(home) = dirs::home_dir() {
            let win_path = home
                .join("AppData")
                .join("Roaming")
                .join("Claude")
                .join("claude_desktop_config.json");
            if !paths.contains(&win_path) {
                paths.push(win_path);
            }
            let mac_path = home
                .join("Library")
                .join("Application Support")
                .join("Claude")
                .join("claude_desktop_config.json");
            if !paths.contains(&mac_path) {
                paths.push(mac_path);
            }
            let linux_path = home
                .join(".config")
                .join("Claude")
                .join("claude_desktop_config.json");
            if !paths.contains(&linux_path) {
                paths.push(linux_path);
            }
        }

        paths
    }

    /// Parses a JSON config file containing `mcpServers`
    pub fn parse_config_file(
        path: &std::path::Path,
        source_label: &str,
    ) -> Vec<DiscoveredMcpServer> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                debug!(path = %path.display(), error = %e, "Failed to read MCP config file");
                return Vec::new();
            }
        };

        Self::parse_config_str(&content, source_label)
    }

    /// Parses a raw JSON string containing `mcpServers`
    pub fn parse_config_str(content: &str, source_label: &str) -> Vec<DiscoveredMcpServer> {
        let parsed: RawMcpConfigFile = match serde_json::from_str(content) {
            Ok(p) => p,
            Err(e) => {
                debug!(error = %e, source = %source_label, "Failed to parse JSON MCP config");
                return Vec::new();
            }
        };

        let mut servers = Vec::new();
        for (name, entry) in parsed.mcp_servers {
            if entry.disabled.unwrap_or(false) {
                continue;
            }

            // Determine transport: stdio vs sse/http
            let transport = entry.transport.clone().unwrap_or_else(|| {
                if entry.url.is_some() {
                    "http".to_string()
                } else {
                    "stdio".to_string()
                }
            });

            servers.push(DiscoveredMcpServer {
                name,
                transport,
                command: entry.command,
                args: entry.args,
                env: entry.env,
                url: entry.url,
                auth_token: entry.auth_token,
                source: source_label.to_string(),
            });
        }

        servers
    }

    /// Auto-discovers and connects to all discovered MCP servers (stdio and http/sse)
    pub async fn auto_discover_and_connect(&mut self) -> Result<Vec<DiscoveredMcpServer>> {
        let discovered = Self::discover_configs();
        for s in &discovered {
            // Only connect if not already present
            if self.servers.contains_key(&s.name) {
                continue;
            }

            let transport_norm = s.transport.to_ascii_lowercase();
            if transport_norm == "stdio" {
                if let Some(cmd) = &s.command {
                    if let Err(e) = self
                        .add_stdio_server(
                            s.name.clone(),
                            cmd.clone(),
                            s.args.clone(),
                            s.env.clone(),
                        )
                        .await
                    {
                        warn!(server = %s.name, error = %e, "Failed to connect to discovered stdio MCP server");
                    } else {
                        info!(server = %s.name, source = %s.source, "Connected to discovered stdio MCP server");
                    }
                }
            } else if transport_norm == "http" || transport_norm == "sse" {
                if let Some(url) = &s.url {
                    if let Err(e) = self
                        .add_server(s.name.clone(), url.clone(), s.auth_token.clone())
                        .await
                    {
                        warn!(server = %s.name, error = %e, "Failed to connect to discovered HTTP/SSE MCP server");
                    } else {
                        info!(server = %s.name, source = %s.source, "Connected to discovered HTTP/SSE MCP server");
                    }
                }
            }
        }

        Ok(discovered)
    }
}

impl McpManager {
    /// Create a new MCP manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Add and connect to an HTTP MCP server
    pub async fn add_server(
        &mut self,
        name: impl Into<String>,
        url: String,
        auth_token: Option<String>,
    ) -> Result<()> {
        let name = name.into();
        let client = McpClient::new(url, auth_token);
        client.connect().await?;
        self.servers.insert(name, McpTransport::Http(client));
        Ok(())
    }

    /// Add and connect to a stdio MCP server (child process)
    pub async fn add_stdio_server(
        &mut self,
        name: impl Into<String>,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Result<()> {
        let name = name.into();
        let client = McpStdioClient::new(command, args, env);
        client.connect().await?;
        self.servers.insert(name, McpTransport::Stdio(client));
        Ok(())
    }

    /// Remove and disconnect a server
    pub async fn remove_server(&mut self, name: &str) -> Result<()> {
        if let Some(transport) = self.servers.remove(name) {
            transport.disconnect().await?;
        }
        Ok(())
    }

    /// Get a server transport by name
    pub fn get(&self, name: &str) -> Option<&McpTransport> {
        self.servers.get(name)
    }

    /// Get all servers
    pub fn servers(&self) -> &HashMap<String, McpTransport> {
        &self.servers
    }

    /// Get all tools from all servers
    pub async fn get_all_tools(&self) -> Vec<McpTool> {
        let mut tools = Vec::new();
        for transport in self.servers.values() {
            if transport.is_connected().await {
                tools.extend(transport.get_tools().await.iter().cloned());
            }
        }
        tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolContext, ToolRegistry};
    use mockito::Matcher;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn test_tool_definition() {
        let def = McpToolDefinition {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                }
            }),
        };

        assert_eq!(def.name, "test_tool");
    }

    #[tokio::test]
    async fn test_mcp_manager_empty() {
        let manager = McpManager::new();
        assert!(manager.servers.is_empty());
    }

    #[tokio::test]
    async fn http_client_connects_lists_and_calls_tool() {
        let mut server = mockito::Server::new_async().await;
        let initialize = server
            .mock("POST", "/")
            .match_header("authorization", "Bearer test-token")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialize"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocol_version": MCP_VERSION,
                        "capabilities": { "tools": {} },
                        "server_info": { "name": "mock", "version": "1.0.0" }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;
        let initialized = server
            .mock("POST", "/")
            .match_header("authorization", "Bearer test-token")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "id": 0
            })))
            .with_status(200)
            .create_async()
            .await;
        let tools_list = server
            .mock("POST", "/")
            .match_header("authorization", "Bearer test-token")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "tools/list"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "description": "Echoes text",
                            "input_schema": {
                                "type": "object",
                                "properties": { "text": { "type": "string" } }
                            }
                        }]
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;
        let tools_call = server
            .mock("POST", "/")
            .match_header("authorization", "Bearer test-token")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {
                    "name": "echo",
                    "arguments": { "text": "hello" }
                }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "result": { "content": [{ "type": "text", "text": "hello" }] }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = McpClient::new(server.url(), Some("test-token".to_string()));
        client.connect().await.unwrap();

        assert!(client.is_connected().await);
        let caps = client.get_capabilities().await;
        assert!(caps.tools);
        let tools = client.get_tools().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "echo");

        let result = client
            .call_tool("echo", serde_json::json!({ "text": "hello" }))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "hello");

        initialize.assert_async().await;
        initialized.assert_async().await;
        tools_list.assert_async().await;
        tools_call.assert_async().await;
    }

    #[tokio::test]
    async fn manager_exposes_http_mcp_tools_to_registry() {
        let mut server = mockito::Server::new_async().await;
        let _initialize = server
            .mock("POST", "/")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "method": "initialize"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocol_version": MCP_VERSION,
                        "capabilities": { "tools": {} },
                        "server_info": { "name": "mock", "version": "1.0.0" }
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;
        let _initialized = server
            .mock("POST", "/")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "method": "initialized"
            })))
            .with_status(200)
            .create_async()
            .await;
        let _tools_list = server
            .mock("POST", "/")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "method": "tools/list"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "tools": [{
                            "name": "remote_echo",
                            "description": "Echoes text remotely",
                            "input_schema": { "type": "object", "properties": {} }
                        }]
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;
        let tools_call = server
            .mock("POST", "/")
            .match_body(Matcher::PartialJson(serde_json::json!({
                "method": "tools/call",
                "params": {
                    "name": "remote_echo",
                    "arguments": { "text": "hello" }
                }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "result": { "content": [{ "type": "text", "text": "hello" }] }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let mut manager = McpManager::new();
        manager
            .add_server("mock", server.url(), None)
            .await
            .unwrap();
        let tools = manager.get_all_tools().await;

        let registry = ToolRegistry::new(Duration::from_secs(1));
        for tool in tools {
            registry.register(tool).await.unwrap();
        }

        assert!(registry.contains("remote_echo").await);
        let schemas = registry.get_schemas().await;
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "remote_echo");

        let result = registry
            .execute(
                "remote_echo",
                "call_1",
                serde_json::json!({ "text": "hello" }),
                ToolContext::default(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.content.contains("hello"));
        tools_call.assert_async().await;
    }

    /// Resolve a Python interpreter for the fake stdio server. Distros
    /// differ: some ship only `python3`, some only `python`. Returns None
    /// when neither exists so the test can skip instead of failing.
    fn python_interpreter() -> Option<String> {
        for candidate in ["python3", "python"] {
            if std::process::Command::new(candidate)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return Some(candidate.to_string());
            }
        }
        None
    }

    #[tokio::test]
    async fn stdio_client_connects_lists_and_calls_tool() {
        let Some(python) = python_interpreter() else {
            eprintln!("skipping: no python interpreter found");
            return;
        };
        let server_path = fake_stdio_server_path();
        let client = McpStdioClient::new(&python, vec![server_path], HashMap::new());

        client.connect().await.unwrap();

        assert!(client.is_connected().await);
        let caps = client.get_capabilities().await;
        assert!(caps.tools);
        let tools = client.get_tools().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "stdio_echo");

        let result = client
            .call_tool("stdio_echo", serde_json::json!({ "text": "hello" }))
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "hello");

        client.disconnect().await.unwrap();
    }

    fn fake_stdio_server_path() -> String {
        let dir = std::env::temp_dir().join(format!("kerux-mcp-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fake_mcp_server.py");
        fs::write(
            &path,
            r#"
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialized":
        continue
    if method == "initialize":
        result = {
            "protocol_version": "2024-11-05",
            "capabilities": {"tools": {}},
            "server_info": {"name": "fake-stdio", "version": "1.0.0"},
        }
    elif method == "tools/list":
        result = {
            "tools": [{
                "name": "stdio_echo",
                "description": "Echoes text over stdio",
                "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}},
            }]
        }
    elif method == "tools/call":
        text = request.get("params", {}).get("arguments", {}).get("text", "")
        result = {"content": [{"type": "text", "text": text}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request.get("id"), "result": result}), flush=True)
"#,
        )
        .unwrap();

        path.to_string_lossy().into_owned()
    }

    #[test]
    fn test_parse_mcp_config_json() {
        let sample = r#"{
            "mcpServers": {
                "fetch": {
                    "command": "uvx",
                    "args": ["mcp-server-fetch"],
                    "env": {"DEBUG": "1"}
                },
                "remote_api": {
                    "url": "https://mcp.example.com/sse",
                    "authToken": "secret-token",
                    "transport": "sse"
                },
                "disabled_server": {
                    "command": "node",
                    "args": ["server.js"],
                    "disabled": true
                }
            }
        }"#;

        let servers = McpManager::parse_config_str(sample, "test-config");
        assert_eq!(servers.len(), 2);

        let fetch = servers.iter().find(|s| s.name == "fetch").unwrap();
        assert_eq!(fetch.transport, "stdio");
        assert_eq!(fetch.command.as_deref(), Some("uvx"));
        assert_eq!(fetch.args, vec!["mcp-server-fetch".to_string()]);
        assert_eq!(fetch.env.get("DEBUG").map(|s| s.as_str()), Some("1"));
        assert_eq!(fetch.source, "test-config");

        let remote = servers.iter().find(|s| s.name == "remote_api").unwrap();
        assert_eq!(remote.transport, "sse");
        assert_eq!(remote.url.as_deref(), Some("https://mcp.example.com/sse"));
        assert_eq!(remote.auth_token.as_deref(), Some("secret-token"));

        assert!(servers.iter().all(|s| s.name != "disabled_server"));
    }

    #[test]
    fn test_claude_desktop_paths() {
        let paths = McpManager::claude_desktop_config_paths();
        assert!(!paths.is_empty());
        for p in &paths {
            assert!(p.to_string_lossy().contains("claude_desktop_config.json"));
        }
    }
}
