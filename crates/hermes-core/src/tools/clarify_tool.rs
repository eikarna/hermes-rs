//! Clarify tool
//!
//! A tool for the agent to ask the user clarifying questions.
//! Matches Python's clarify_tool.py. Returns a structured question
//! for the frontend/CLI layer to present to the user.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use schemars::JsonSchema;

use crate::schema::ToolSchema;
use crate::tools::{HermesTool, ToolContext, ToolResult};

/// Maximum number of choices allowed
const MAX_CHOICES: usize = 4;

/// Arguments for the clarify tool
#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClarifyArgs {
    /// The clarifying question to ask the user
    question: String,
    /// Optional list of choices (max 4) for the user to pick from
    choices: Option<Vec<String>>,
}

/// Tool for asking the user clarifying questions
pub struct ClarifyTool;

#[async_trait]
impl HermesTool for ClarifyTool {
    fn name(&self) -> &str {
        "clarify"
    }

    fn description(&self) -> &str {
        "Ask the user a clarifying question when you need more information to proceed. \
        Optionally provide up to 4 choices for the user to select from."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<ClarifyArgs>("clarify", "Ask the user a clarifying question")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: ClarifyArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("clarify", format!("Invalid arguments: {}", e)),
        };

        if args.question.trim().is_empty() {
            return ToolResult::error("clarify", "Question must not be empty");
        }

        let choices = match args.choices {
            Some(ref c) if c.len() > MAX_CHOICES => {
                return ToolResult::error(
                    "clarify",
                    format!("Too many choices: {} provided, maximum is {}", c.len(), MAX_CHOICES),
                );
            }
            Some(c) => Some(c),
            None => None,
        };

        let mut response = serde_json::json!({
            "type": "clarification",
            "question": args.question,
        });

        if let Some(ref choices) = choices {
            response["choices"] = serde_json::json!(choices);
            response["choiceCount"] = serde_json::json!(choices.len());
        }

        ToolResult::success("clarify", response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    fn default_context() -> ToolContext {
        ToolContext::default()
    }

    #[tokio::test]
    async fn test_clarify_simple_question() {
        let tool = ClarifyTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "question": "Which database do you want to use?"
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["type"], "clarification");
        assert_eq!(parsed["question"], "Which database do you want to use?");
        assert!(parsed.get("choices").is_none());
    }

    #[tokio::test]
    async fn test_clarify_with_choices() {
        let tool = ClarifyTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "question": "Pick a framework:",
                    "choices": ["React", "Vue", "Svelte"]
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["choiceCount"], 3);
        assert_eq!(parsed["choices"][0], "React");
    }

    #[tokio::test]
    async fn test_clarify_too_many_choices() {
        let tool = ClarifyTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "question": "Pick one:",
                    "choices": ["A", "B", "C", "D", "E"]
                }),
                default_context(),
            )
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("Too many choices"));
    }

    #[tokio::test]
    async fn test_clarify_empty_question() {
        let tool = ClarifyTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "question": "   "
                }),
                default_context(),
            )
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn test_clarify_max_choices_allowed() {
        let tool = ClarifyTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "question": "Pick one:",
                    "choices": ["A", "B", "C", "D"]
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["choiceCount"], 4);
    }
}
