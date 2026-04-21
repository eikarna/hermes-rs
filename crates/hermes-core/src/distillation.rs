//! State distillation for durable long-term memory.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::debug;

use crate::client::{Message, OpenAIClient, Role};
use crate::error::{Error, Result};
use crate::memory::{MemoryBlock, MemoryManager};

const DISTILLATION_SYSTEM_DIRECTIVE: &str = "Analyze the conversation history. Extract ONLY permanent, durable knowledge, rules, and user preferences. Ignore ephemeral bugs, narrative, or code snippets. Output ONLY a JSON array of strings containing these concise facts.";

/// Distill durable facts from a completed session and persist them to long-term memory.
pub async fn distill_session_to_memory(
    client: OpenAIClient,
    model: String,
    memory_manager: MemoryManager,
    history: Vec<Message>,
) -> Result<usize> {
    if history.is_empty() {
        return Ok(0);
    }

    let transcript = format_history_for_distillation(&history);
    if transcript.trim().is_empty() {
        return Ok(0);
    }

    let messages = vec![
        Message::system(DISTILLATION_SYSTEM_DIRECTIVE),
        Message::user(format!("Conversation history:\n{}", transcript)),
    ];
    let response = client.chat(&model, &messages, None).await?;
    let content = response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .ok_or_else(|| Error::ParseResponse("Distillation response had no content".to_string()))?;

    let facts = parse_distilled_facts(&content)?;
    let mut stored = 0;
    let mut seen = HashSet::new();

    for fact in facts {
        let trimmed = fact.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }

        let block = MemoryBlock::new(
            distilled_memory_id(trimmed, stored),
            "fact",
            trimmed.to_string(),
        )
        .importance(90)
        .tags(vec!["distilled".to_string(), "long_term".to_string()]);
        memory_manager.store(block).await;
        stored += 1;
    }

    memory_manager
        .save_to_disk()
        .await
        .map_err(|error| Error::Agent(format!("Failed to persist distilled memory: {}", error)))?;

    debug!(stored, "Distilled session facts into long-term memory");
    Ok(stored)
}

fn parse_distilled_facts(raw: &str) -> Result<Vec<String>> {
    serde_json::from_str::<Vec<String>>(raw.trim())
        .map_err(|error| Error::ParseResponse(format!("Invalid distillation JSON: {}", error)))
}

fn format_history_for_distillation(history: &[Message]) -> String {
    history
        .iter()
        .filter(|message| message.role != Role::System && !message.content.trim().is_empty())
        .map(|message| format!("{}: {}", message.role.as_str(), message.content.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn distilled_memory_id(fact: &str, index: usize) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fact.hash(&mut hasher);
    format!("distilled_{}_{}_{}", now, index, hasher.finish())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mockito::Server;

    use super::*;
    use crate::client::ClientConfig;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hermes_distillation_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir should be created");
        dir
    }

    #[test]
    fn parses_strict_json_array() {
        let facts = parse_distilled_facts("[\"User prefers concise answers\"]").unwrap();
        assert_eq!(facts, vec!["User prefers concise answers"]);
    }

    #[test]
    fn rejects_non_array_distillation_output() {
        let error = parse_distilled_facts("{\"fact\":\"nope\"}").unwrap_err();
        assert!(error.to_string().contains("Invalid distillation JSON"));
    }

    #[tokio::test]
    async fn distills_facts_and_persists_memory() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "id": "distill_1",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "demo",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "[\"User prefers concise answers\", \"Project uses Rust\"]"
                        },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = OpenAIClient::new(ClientConfig {
            base_url: format!("{}/v1", server.url()),
            api_key: None,
            timeout: Duration::from_secs(5),
            max_context_length: 128_000,
        });
        let dir = test_dir("persist");
        let memory_manager = MemoryManager::with_storage_dir(dir.clone());

        let stored = distill_session_to_memory(
            client,
            "demo".to_string(),
            memory_manager.clone(),
            vec![
                Message::user("Please be concise in future answers."),
                Message::assistant("Understood."),
            ],
        )
        .await
        .unwrap();

        assert_eq!(stored, 2);
        assert_eq!(memory_manager.search("concise").await.len(), 1);
        assert!(dir.join("MEMORY.md").exists());

        let _ = std::fs::remove_dir_all(dir);
    }
}
