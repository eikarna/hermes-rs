use crate::mock_env::{MockEnvironment, ToolResponse};
use crate::workspace::SyntheticWorkspace;
use std::time::Instant;

pub struct ScoringRubric {
    pub execution_success: bool,
    pub token_efficiency: f32,
    pub time_to_first_working_diff: std::time::Duration,
    pub tool_calling_accuracy: f32,
}

pub struct BenchmarkEngine {
    workspace: SyntheticWorkspace,
    env: MockEnvironment,
}

impl BenchmarkEngine {
    pub fn new(workspace: SyntheticWorkspace) -> Self {
        Self {
            workspace,
            env: MockEnvironment::new(),
        }
    }

    pub fn run_evaluation(&mut self) -> ScoringRubric {
        let start_time = Instant::now();
        let mut tokens_used = 0;
        let mut valid_tool_calls = 0;
        let mut total_tool_calls = 0;

        // Simulated Agent Loop
        for _ in 0..5 {
            total_tool_calls += 1;
            tokens_used += 1500; // Simulated token cost per turn

            let response = self.env.execute_tool("patch_file", "{}");
            match response {
                ToolResponse::Success(_) => {
                    valid_tool_calls += 1;
                    // Simulate the agent successfully patching the file
                    std::fs::write(
                        self.workspace.root.join("calc.rs"),
                        "pub fn calculate_total(items: &[i32], discount: i32) -> i32 { let mut total = items.iter().sum::<i32>(); if discount > 0 { total -= discount; } total }"
                    ).unwrap();
                    break;
                }
                _ => {
                    // Agent must handle rate limits and malformed JSON and retry
                }
            }
        }

        ScoringRubric {
            execution_success: self.workspace.verify_fix(),
            token_efficiency: valid_tool_calls as f32 / tokens_used as f32,
            time_to_first_working_diff: start_time.elapsed(),
            tool_calling_accuracy: valid_tool_calls as f32 / total_tool_calls as f32,
        }
    }
}
