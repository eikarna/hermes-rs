//! MCP (Model Context Protocol) client for Hermes-RS
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
use crate::tools::{HermesTool, ToolContext, ToolResult};

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
    tools: Arc<RwLock<Vec<McpTool>>>,
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
            tools: Arc::new(RwLock::new(Vec::new())),
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
                name: "hermes-rs".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let response = self.send_request("initialize", Some(serde_json::to_value(request)?)).await?;

        let init_response: InitializeResponse = serde_json::from_value(response)
            .map_err(|e| crate::error::Error::ParseResponse(format!("Failed to parse initialize response: {}", e)))?;

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
        self.tools.write().await.clear();
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
        let tool_list: ToolListResult = serde_json::from_value(response)
            .map_err(|e| crate::error::Error::ParseResponse(format!("Failed to parse tool list: {}", e)))?;

        let tools: Vec<McpTool> = tool_list.tools
            .into_iter()
            .map(|def| McpTool::new(self.clone(), def))
            .collect();

        *self.tools.write().await = tools;

        debug!(count = self.tools.read().await.len(), "Listed MCP tools");
        Ok(self.tools.read().await.iter().map(|t| t.definition.clone()).collect())
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
    pub async fn get_tools(&self) -> Vec<McpTool> {
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

        let mut req_builder = self.client
            .post(&self.url)
            .header("Content-Type", "application/json");

        if let Some(ref token) = self.auth_token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder
            .json(&request)
            .send()
            .await?;

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

        rpc_response.result
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

        let mut req_builder = self.client
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
    tools: Arc<RwLock<Vec<McpTool>>>,
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
            tools: Arc::new(RwLock::new(Vec::new())),
            capabilities: Arc::new(RwLock::new(McpCapabilities::default())),
            connected: Arc::new(RwLock::new(false)),
            request_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Connect to the MCP server by spawning the child process and initializing
    pub async fn connect(&self) -> Result<()> {
        info!(command = %self.command, "Spawning MCP stdio server");

        let mut cmd = tokio::process::Command::new(&self.command);
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
                name: "hermes-rs".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let response = self
            .send_request("initialize", Some(serde_json::to_value(request)?))
            .await?;

        let init_response: InitializeResponse = serde_json::from_value(response).map_err(
            |e| {
                crate::error::Error::ParseResponse(format!(
                    "Failed to parse initialize response: {}",
                    e
                ))
            },
        )?;

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
        self.tools.write().await.clear();

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

        *self.tools.write().await = tools;

        debug!(count = self.tools.read().await.len(), "Listed MCP stdio tools");
        Ok(self
            .tools
            .read()
            .await
            .iter()
            .map(|t| t.definition.clone())
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
    pub async fn get_tools(&self) -> Vec<McpTool> {
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
        io.stdin.flush().await.map_err(|e| {
            crate::error::Error::Agent(format!("Failed to flush MCP stdin: {}", e))
        })?;

        // Read response from stdout
        let mut response_line = String::new();
        io.stdout
            .read_line(&mut response_line)
            .await
            .map_err(|e| {
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
    pub async fn get_tools(&self) -> Vec<McpTool> {
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
    definition: McpToolDefinition,
}

impl McpTool {
    /// Create a new MCP tool wrapper (HTTP transport)
    pub fn new(client: McpClient, definition: McpToolDefinition) -> Self {
        Self {
            transport: McpTransport::Http(client),
            definition,
        }
    }

    /// Create a new MCP tool wrapper (stdio transport)
    pub fn new_stdio(client: McpStdioClient, definition: McpToolDefinition) -> Self {
        Self {
            transport: McpTransport::Stdio(client),
            definition,
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
impl HermesTool for McpTool {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn schema(&self) -> ToolSchema {
        let params = serde_json::to_value(&self.definition.input_schema)
            .unwrap_or_else(|_| serde_json::json!({"type": "object"}));

        ToolSchema::new(
            &self.definition.name,
            &self.definition.description,
            params,
        )
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

impl McpManager {
    /// Create a new MCP manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Add and connect to an HTTP MCP server
    pub async fn add_server(&mut self, name: impl Into<String>, url: String, auth_token: Option<String>) -> Result<()> {
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
                tools.extend(transport.get_tools().await);
            }
        }
        tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}