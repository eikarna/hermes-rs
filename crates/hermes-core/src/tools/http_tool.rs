//! HTTP request tool
//!
//! Tool for making HTTP requests with full control over headers, method, and body.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

use crate::config::runtime_config;
use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolResult};

/// Tool for making HTTP requests
pub struct HttpRequestTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpRequestArgs {
    url: String,
    method: Option<String>,
    headers: Option<HashMap<String, String>>,
    body: Option<String>,
    timeout: Option<u64>,
}

#[async_trait]
impl HermesTool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn description(&self) -> &str {
        "Make an HTTP request with full control over method, headers, and body. Supports GET, POST, PUT, DELETE, PATCH, HEAD."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<HttpRequestArgs>("http_request", "Make HTTP request")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: HttpRequestArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolResult::error("http_request", format!("Invalid arguments: {}", e))
            }
        };

        // Validate URL
        let parsed_url = match reqwest::Url::parse(&args.url) {
            Ok(u) => u,
            Err(e) => return ToolResult::error("http_request", format!("Invalid URL: {}", e)),
        };

        if parsed_url.scheme() != "http" && parsed_url.scheme() != "https" {
            return ToolResult::error("http_request", "Only HTTP and HTTPS URLs are supported");
        }

        let timeout = std::time::Duration::from_secs(
            args.timeout
                .unwrap_or(runtime_config().tools.http.timeout_secs),
        );

        let client = match reqwest::Client::builder().timeout(timeout).build() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error("http_request", format!("Failed to create client: {}", e))
            }
        };

        let method = args.method.as_deref().unwrap_or("GET").to_uppercase();
        let mut request = match method.as_str() {
            "GET" => client.get(&args.url),
            "POST" => client.post(&args.url),
            "PUT" => client.put(&args.url),
            "DELETE" => client.delete(&args.url),
            "PATCH" => client.patch(&args.url),
            "HEAD" => client.head(&args.url),
            "OPTIONS" => client.request(reqwest::Method::OPTIONS, &args.url),
            _ => {
                return ToolResult::error(
                    "http_request",
                    format!("Unsupported HTTP method: {}", method),
                )
            }
        };

        // Add headers
        if let Some(ref headers) = args.headers {
            for (key, value) in headers {
                request = request.header(key, value);
            }
        }

        // Add body for methods that support it
        if let Some(ref body) = args.body {
            if !body.is_empty() {
                request = request.body(body.clone());
            }
        }

        let start = std::time::Instant::now();

        match request.send().await {
            Ok(response) => {
                let elapsed = start.elapsed();
                let status = response.status();
                let version = format!("{:?}", response.version());

                // Collect response headers
                let response_headers: HashMap<String, String> = response
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();

                // Get body
                let body_size = response.content_length();
                let body = response.text().await.ok();

                ToolResult::success(
                    "http_request",
                    serde_json::json!({
                        "url": args.url,
                        "method": method,
                        "status_code": status.as_u16(),
                        "status_text": status.canonical_reason().unwrap_or(""),
                        "version": version,
                        "headers": response_headers,
                        "body": body,
                        "body_size": body_size,
                        "response_time_ms": elapsed.as_millis() as u64
                    }),
                )
            }
            Err(e) => {
                let error_type = if e.is_timeout() {
                    "timeout"
                } else if e.is_connect() {
                    "connection_error"
                } else {
                    "request_error"
                };

                ToolResult::error(
                    "http_request",
                    serde_json::json!({
                        "error": e.to_string(),
                        "error_type": error_type,
                        "url": args.url,
                        "method": method
                    })
                    .to_string(),
                )
            }
        }
    }
}
