//! Sub-agent delegation tool.
//!
//! This tool lets a parent agent delegate focused analysis to an isolated child
//! agent without changing the parent ReAct loop.

use std::error::Error as StdError;

use async_trait::async_trait;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::agent::{AgentConfig, HermesAgent};
use crate::client::{ClientConfig, OpenAIClient};
use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolRegistry, ToolResult};

const TOOL_NAME: &str = "delegate_to_sub_agent";
const SUB_AGENT_SYSTEM_PROMPT: &str = "\
You are a focused Hermes sub-agent. You receive one delegated task from a parent agent.
Perform deep reasoning, codebase analysis, or specialized implementation planning as requested.
Do not assume access to the parent conversation or long-term memory.
Return only the concise final findings or result that the parent agent needs.";

type BoxedToolError = Box<dyn StdError + Send + Sync>;

/// Arguments for delegated sub-agent work.
#[derive(Debug, Deserialize, JsonSchema)]
struct SubAgentArgs {
    /// The focused task instruction for the child agent.
    task: String,
}

/// Tool that delegates a focused task to an isolated child HermesAgent.
pub struct SubAgentTool {
    client_config: ClientConfig,
    http_client: Client,
    model: String,
}

impl SubAgentTool {
    pub fn new(parent_client: &OpenAIClient, model: impl Into<String>) -> Self {
        Self {
            client_config: parent_client.config_clone(),
            http_client: parent_client.http_client_clone(),
            model: model.into(),
        }
    }

    /// Run a focused delegated task in an isolated child agent.
    pub async fn call(
        &self,
        task: impl Into<String>,
    ) -> std::result::Result<String, BoxedToolError> {
        self.ensure_supported_model()?;

        let task = task.into();
        let task = task.trim();
        if task.is_empty() {
            return Err("Sub-agent task must not be empty".into());
        }

        let client = OpenAIClient::from_shared_http_client(
            self.client_config.clone(),
            self.http_client.clone(),
        );
        let config = AgentConfig {
            model: self.model.clone(),
            stream: false,
            system_prompt: Some(SUB_AGENT_SYSTEM_PROMPT.to_string()),
            ..AgentConfig::default()
        };

        let registry = ToolRegistry::new(config.tool_timeout);
        let agent = HermesAgent::new(config, client, registry);
        let message = agent
            .run(task.to_string())
            .await
            .map_err(|error| -> BoxedToolError { Box::new(error) })?;

        Ok(message.content)
    }

    fn ensure_supported_model(&self) -> std::result::Result<(), BoxedToolError> {
        if is_llama_model(&self.model) {
            return Err(format!(
                "Sub-agent model '{}' is rejected because Llama-family models are unsuitable for this tool-calling context",
                self.model
            )
            .into());
        }

        Ok(())
    }
}

#[async_trait]
impl HermesTool for SubAgentTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delegate a focused, complex task to an isolated sub-agent. Use this for deep analysis, \
        specialized coding investigation, architectural review, or other self-contained work where \
        the parent agent benefits from a concise expert result. The sub-agent has a fresh \
        conversation and does not inherit parent memory."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<SubAgentArgs>(
            TOOL_NAME,
            "Delegate a focused task to an isolated Hermes sub-agent",
        )
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let task = match parse_task(args) {
            Ok(task) => task,
            Err(error) => return ToolResult::error(TOOL_NAME, error),
        };

        match self.call(task).await {
            Ok(content) => ToolResult {
                tool_call_id: TOOL_NAME.to_string(),
                success: true,
                content,
                error: None,
            },
            Err(error) => ToolResult::error(TOOL_NAME, error.to_string()),
        }
    }
}

fn parse_task(args: Value) -> std::result::Result<String, String> {
    let task = match args {
        Value::String(task) => task,
        value => {
            serde_json::from_value::<SubAgentArgs>(value)
                .map_err(|error| format!("Invalid arguments: {}", error))?
                .task
        }
    };

    let task = task.trim().to_string();
    if task.is_empty() {
        return Err("Task must not be empty".to_string());
    }

    Ok(task)
}

fn is_llama_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("llama")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientConfig;
    use std::time::Duration;

    #[test]
    fn parse_task_accepts_object_argument() {
        let task = parse_task(serde_json::json!({ "task": "analyze this module" })).unwrap();
        assert_eq!(task, "analyze this module");
    }

    #[test]
    fn parse_task_accepts_raw_string_argument() {
        let task = parse_task(Value::String("analyze this module".to_string())).unwrap();
        assert_eq!(task, "analyze this module");
    }

    #[test]
    fn parse_task_rejects_empty_task() {
        let error = parse_task(serde_json::json!({ "task": "  " })).unwrap_err();
        assert_eq!(error, "Task must not be empty");
    }

    #[test]
    fn model_guard_rejects_llama_models() {
        assert!(is_llama_model("meta-llama/Llama-3.1-70B-Instruct"));
        assert!(is_llama_model("llama-3.2"));
    }

    #[test]
    fn model_guard_allows_non_llama_models() {
        assert!(!is_llama_model("gpt-4.1"));
        assert!(!is_llama_model("claude-3-5-sonnet"));
    }

    #[tokio::test]
    async fn call_returns_mocked_child_agent_content() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "id": "chatcmpl-sub-agent",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4.1",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "child final result"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2
                    }
                }"#,
            )
            .create_async()
            .await;

        let parent_client = OpenAIClient::new(ClientConfig {
            base_url: server.url(),
            api_key: None,
            timeout: Duration::from_secs(5),
            max_context_length: 128_000,
        });
        let tool = SubAgentTool::new(&parent_client, "gpt-4.1");

        let result = tool.call("inspect the code").await.unwrap();

        assert_eq!(result, "child final result");
        mock.assert_async().await;
    }
}
