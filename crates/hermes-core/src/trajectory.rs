//! Trajectory saving for RL training data generation
//!
//! Exports conversation trajectories in formats suitable for
//! reinforcement learning training (e.g., RLHF, RLAIF).

use crate::client::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

/// A single step in a trajectory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStep {
    /// Step index in the trajectory
    pub step: usize,
    /// The agent's reasoning (thought process)
    pub thought: Option<String>,
    /// Action taken (tool call name)
    pub action: Option<String>,
    /// Action arguments
    pub action_args: Option<String>,
    /// Observation/result from the action
    pub observation: Option<String>,
    /// Final response if this was the last step
    pub response: Option<String>,
    /// Whether this step was successful
    pub success: bool,
}

/// A complete trajectory (conversation) for training
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// Unique trajectory identifier
    pub id: String,
    /// Session ID this trajectory came from
    pub session_id: String,
    /// Model used
    pub model: String,
    /// Timestamp when trajectory was created
    pub timestamp: i64,
    /// Total tokens used
    pub total_tokens: usize,
    /// Number of tool calls made
    pub tool_calls: usize,
    /// Number of iterations
    pub iterations: usize,
    /// Whether the trajectory was successful
    pub success: bool,
    /// The individual steps
    pub steps: Vec<TrajectoryStep>,
    /// Messages in the conversation
    pub messages: Vec<Message>,
    /// Metadata about the trajectory
    pub metadata: HashMap<String, String>,
}

impl Trajectory {
    /// Create a new trajectory
    pub fn new(
        id: impl Into<String>,
        session_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            id: id.into(),
            session_id: session_id.into(),
            model: model.into(),
            timestamp,
            total_tokens: 0,
            tool_calls: 0,
            iterations: 0,
            success: false,
            steps: Vec::new(),
            messages: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a step to the trajectory
    pub fn add_step(&mut self, step: TrajectoryStep) {
        self.steps.push(step);
    }

    /// Add a message to the trajectory
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Set success status
    pub fn set_success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    /// Set metadata
    pub fn set_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Calculate total tokens from all messages
    pub fn calculate_tokens(&mut self) {
        self.total_tokens = self
            .messages
            .iter()
            .map(|m| crate::context::estimate_message_tokens(m))
            .sum();
    }

    /// Convert to JSON string
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Convert to compact JSON string
    pub fn to_json_compact(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Export as a prompt-completion pair for language model fine-tuning
    pub fn to_prompt_completion(&self) -> Option<(String, String)> {
        let mut prompt = String::new();
        let mut completion = String::new();

        for step in &self.steps {
            if let Some(ref thought) = step.thought {
                prompt.push_str(&format!("Thought: {}\n", thought));
            }
            if let Some(ref action) = step.action {
                prompt.push_str(&format!("Action: {}\n", action));
                if let Some(ref args) = step.action_args {
                    prompt.push_str(&format!("Action Input: {}\n", args));
                }
            }
            if let Some(ref obs) = step.observation {
                completion.push_str(&format!("Observation: {}\n", obs));
            }
            if let Some(ref response) = step.response {
                completion.push_str(&format!("Final Response: {}\n", response));
            }
        }

        if prompt.is_empty() || completion.is_empty() {
            None
        } else {
            Some((prompt, completion))
        }
    }
}

/// Builder for creating trajectories from agent runs
#[derive(Debug)]
pub struct TrajectoryBuilder {
    session_id: String,
    model: String,
    steps: Vec<TrajectoryStep>,
    messages: Vec<Message>,
    metadata: HashMap<String, String>,
}

impl TrajectoryBuilder {
    /// Create a new trajectory builder
    pub fn new(session_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            model: model.into(),
            steps: Vec::new(),
            messages: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a reasoning step
    pub fn add_reasoning_step(
        mut self,
        thought: impl Into<String>,
        action: impl Into<String>,
        action_args: impl Into<String>,
        observation: impl Into<String>,
        success: bool,
    ) -> Self {
        let step = TrajectoryStep {
            step: self.steps.len(),
            thought: Some(thought.into()),
            action: Some(action.into()),
            action_args: Some(action_args.into()),
            observation: Some(observation.into()),
            response: None,
            success,
        };
        self.steps.push(step);
        self
    }

    /// Add a final response step
    pub fn add_response_step(mut self, response: impl Into<String>) -> Self {
        let step = TrajectoryStep {
            step: self.steps.len(),
            thought: None,
            action: None,
            action_args: None,
            observation: None,
            response: Some(response.into()),
            success: true,
        };
        self.steps.push(step);
        self
    }

    /// Add a message to the trajectory
    pub fn add_message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Set metadata
    pub fn set_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Build the trajectory
    pub fn build(self) -> Trajectory {
        let mut trajectory = Trajectory::new(
            format!("traj_{}", uuid_simple()),
            &self.session_id,
            &self.model,
        );

        for step in self.steps {
            trajectory.add_step(step);
        }

        for message in self.messages {
            trajectory.add_message(message);
        }

        trajectory.metadata = self.metadata;
        trajectory.calculate_tokens();
        trajectory.iterations = trajectory.steps.len();
        trajectory.tool_calls = trajectory
            .steps
            .iter()
            .filter(|s| s.action.is_some())
            .count();

        trajectory
    }
}

/// Trajectory exporter for saving training data
#[derive(Debug)]
pub struct TrajectoryExporter {
    trajectories: Vec<Trajectory>,
}

impl Default for TrajectoryExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl TrajectoryExporter {
    /// Create a new exporter
    pub fn new() -> Self {
        Self {
            trajectories: Vec::new(),
        }
    }

    /// Add a trajectory to the exporter
    pub fn add(&mut self, trajectory: Trajectory) {
        info!(trajectory_id = %trajectory.id, "Adding trajectory to exporter");
        self.trajectories.push(trajectory);
    }

    /// Export all trajectories as JSON
    pub fn export_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.trajectories)
    }

    /// Export all trajectories as NDJSON (newline-delimited JSON)
    pub fn export_ndjson(&self) -> String {
        self.trajectories
            .iter()
            .map(|t| serde_json::to_string(t).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Export as prompt-completion pairs
    pub fn export_prompt_completion(&self) -> Vec<(String, String)> {
        self.trajectories
            .iter()
            .filter_map(|t| t.to_prompt_completion())
            .collect()
    }

    /// Get count of trajectories
    pub fn len(&self) -> usize {
        self.trajectories.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.trajectories.is_empty()
    }

    /// Clear all trajectories
    pub fn clear(&mut self) {
        self.trajectories.clear();
    }
}

/// Generate a simple unique ID
fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}-{:x}", now.as_secs(), now.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trajectory_builder() {
        let trajectory = TrajectoryBuilder::new("session1", "gpt-4")
            .add_reasoning_step(
                "I need to calculate 15 + 27",
                "calculate",
                r#"{"operation": "add", "a": 15, "b": 27}"#,
                r#"{"result": 42}"#,
                true,
            )
            .add_response_step("The result of 15 + 27 is 42.")
            .build();

        assert_eq!(trajectory.model, "gpt-4");
        assert_eq!(trajectory.steps.len(), 2);
        assert_eq!(trajectory.iterations, 2);
        assert_eq!(trajectory.tool_calls, 1);
    }

    #[test]
    fn test_prompt_completion() {
        let trajectory = TrajectoryBuilder::new("session1", "gpt-4")
            .add_reasoning_step(
                "Let me search for information",
                "web_search",
                r#"{"query": "rust programming"}"#,
                "Found 100 results",
                true,
            )
            .add_response_step("Rust is a systems programming language.")
            .build();

        let (prompt, completion) = trajectory.to_prompt_completion().unwrap();
        assert!(prompt.contains("Thought:"));
        assert!(prompt.contains("Action:"));
        assert!(completion.contains("Observation:"));
        assert!(completion.contains("Final Response:"));
    }

    #[test]
    fn test_trajectory_exporter() {
        let mut exporter = TrajectoryExporter::new();

        exporter.add(
            TrajectoryBuilder::new("s1", "gpt-4")
                .add_response_step("Hello")
                .build(),
        );
        exporter.add(
            TrajectoryBuilder::new("s2", "gpt-4")
                .add_response_step("Hi")
                .build(),
        );

        assert_eq!(exporter.len(), 2);

        let json = exporter.export_json().unwrap();
        assert!(json.contains("traj_"));

        let ndjson = exporter.export_ndjson();
        assert!(ndjson.contains("\n"));
    }
}
