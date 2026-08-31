#[derive(Debug, Clone)]
pub enum ToolResponse {
    Success(String),
    MalformedJson(String),
    RateLimit,
}

pub struct MockEnvironment {
    invocation_count: usize,
}

impl MockEnvironment {
    pub fn new() -> Self {
        Self {
            invocation_count: 0,
        }
    }

    /// Simulates a tool call, intentionally returning bad data to test repair loops.
    pub fn execute_tool(&mut self, _tool_name: &str, _args: &str) -> ToolResponse {
        self.invocation_count += 1;

        match self.invocation_count {
            // First attempt: simulate a rate limit
            1 => ToolResponse::RateLimit,
            // Second attempt: simulate a malformed API response
            2 => ToolResponse::MalformedJson(
                r#"{"result": "success", "data": [missing_bracket}"#.into(),
            ),
            // Third attempt: success
            _ => ToolResponse::Success(r#"{"result": "success", "data": []}"#.into()),
        }
    }
}

impl Default for MockEnvironment {
    fn default() -> Self {
        Self::new()
    }
}
