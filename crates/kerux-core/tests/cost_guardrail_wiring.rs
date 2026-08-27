use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kerux_core::agent::{AgentConfig, AgentEvent, KeruxAgent};
use kerux_core::client::{
    ChatResponse, ChatStreamResponse, Choice, LLMProvider, Message, MessageDelta,
    ProviderCapabilities, Role, ToolCallDelta, ToolCallFunction, Usage,
};
use kerux_core::config::BudgetSettings;
use kerux_core::cost::BudgetAction;
use kerux_core::schema::ToolSchema;
use kerux_core::tools::{KeruxTool, ToolContext, ToolRegistry, ToolResult};
use kerux_core::{Error, Result};
use tokio::sync::mpsc;

struct ContinueTool;

#[async_trait]
impl KeruxTool for ContinueTool {
    fn name(&self) -> &str {
        "continue_run"
    }

    fn description(&self) -> &str {
        "Advances the scripted integration run"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            self.name(),
            self.description(),
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }

    async fn execute(&self, _args: serde_json::Value, _context: ToolContext) -> ToolResult {
        ToolResult::success("provider-call", serde_json::json!({"continued": true}))
    }
}

struct RoutingProvider {
    calls: AtomicUsize,
    models: Arc<Mutex<Vec<String>>>,
}

impl RoutingProvider {
    fn response(
        model: &str,
        content: &str,
        tool_calls: Option<Vec<ToolCallDelta>>,
    ) -> ChatResponse {
        ChatResponse {
            id: "cost-wiring".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: model.to_string(),
            choices: vec![Choice {
                index: 0,
                message: MessageDelta {
                    role: Some(Role::Assistant),
                    content: Some(content.to_string()),
                    reasoning_content: None,
                    tool_calls,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Usage {
                prompt_tokens: 1_000_000,
                completion_tokens: 0,
                total_tokens: 1_000_000,
                cached_prompt_tokens: 0,
            },
        }
    }
}

#[async_trait]
impl LLMProvider for RoutingProvider {
    async fn chat(
        &self,
        model: &str,
        _messages: &[Message],
        _tools: Option<&[ToolSchema]>,
    ) -> Result<ChatResponse> {
        self.models.lock().unwrap().push(model.to_string());
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Ok(Self::response(
                model,
                "",
                Some(vec![ToolCallDelta {
                    index: 0,
                    id: Some("provider-call".to_string()),
                    call_type: Some("function".to_string()),
                    function: Some(ToolCallFunction {
                        name: "continue_run".to_string(),
                        arguments: "{}".to_string(),
                    }),
                }]),
            ));
        }
        Ok(Self::response(model, "finished", None))
    }

    async fn chat_streaming(
        &self,
        _model: &str,
        _messages: &[Message],
        _tools: Option<&[ToolSchema]>,
    ) -> Result<ChatStreamResponse> {
        Err(Error::Agent("streaming not expected".to_string()))
    }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

#[tokio::test]
async fn budget_downgrade_routes_remaining_turns_and_emits_cost() {
    let models = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RoutingProvider {
        calls: AtomicUsize::new(0),
        models: models.clone(),
    });
    let registry = ToolRegistry::new(Duration::from_secs(1));
    registry.register(ContinueTool).await.unwrap();
    let (event_tx, mut event_rx) = mpsc::channel(32);
    let config = AgentConfig {
        model: "expensive-model".to_string(),
        stream: false,
        budget: BudgetSettings {
            enabled: true,
            per_run_limit: 0.5,
            on_limit: "downgrade".to_string(),
            downgrade_model: Some("cheap-model".to_string()),
            ..BudgetSettings::default()
        },
        input_cost_per_million: 1.0,
        output_cost_per_million: 0.0,
        ..AgentConfig::default()
    };
    let agent = KeruxAgent::with_provider_events(config, provider, registry, event_tx);

    let response = agent.run("start".to_string()).await.unwrap();

    assert_eq!(response.content, "finished");
    assert_eq!(
        *models.lock().unwrap(),
        vec!["expensive-model".to_string(), "cheap-model".to_string()]
    );

    let mut downgrade_alerts = 0;
    let mut billable_costs = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        match event {
            AgentEvent::BudgetAlert {
                action: Some(BudgetAction::Downgrade),
                downgrade_model,
                ..
            } => {
                downgrade_alerts += 1;
                assert_eq!(downgrade_model.as_deref(), Some("cheap-model"));
            }
            AgentEvent::Telemetry { telemetry } if telemetry.billable => {
                billable_costs.push(telemetry.estimated_cost_usd);
            }
            _ => {}
        }
    }
    assert_eq!(downgrade_alerts, 1);
    assert_eq!(billable_costs, vec![Some(1.0), Some(1.0)]);
}
