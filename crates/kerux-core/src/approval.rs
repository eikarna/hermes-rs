//! Tool approval gate.
//!
//! Lets a gateway (or any host) require explicit human approval before the
//! agent executes dangerous tools. The agent consults the gate right before
//! running each tool in [`crate::agent::KeruxAgent`]'s execute loop; the
//! gate is responsible for presenting the request (e.g. a Telegram inline
//! keyboard) and resolving it to a decision.

use async_trait::async_trait;

/// Tools that require approval when a gate is installed. Read-only tools
/// (file_read, web_search, ...) never prompt.
pub const DANGEROUS_TOOLS: &[&str] = &[
    "terminal",
    "file_write",
    "patch",
    "edit_block",
    "code_execution",
];

/// Returns true when the named tool is on the approval list.
pub fn requires_approval(tool_name: &str) -> bool {
    DANGEROUS_TOOLS.contains(&tool_name)
}

/// A pending approval request presented to the human.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Tool the agent wants to run.
    pub tool_name: String,
    /// Short human-readable preview of the arguments.
    pub arguments_preview: String,
}

/// Outcome of an approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run the tool.
    Approved,
    /// Do not run the tool; feed `reason` back to the model as the tool error.
    Denied { reason: String },
}

/// Host-side hook the agent calls before executing a dangerous tool.
#[async_trait]
pub trait ToolApprovalGate: Send + Sync {
    /// Present `request` to the human and wait for a decision.
    ///
    /// Implementations must bound their wait (auto-deny on timeout) so a
    /// run can never hang forever on an unanswered prompt.
    async fn request_approval(&self, request: ApprovalRequest) -> ApprovalDecision;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_tools_require_approval() {
        assert!(requires_approval("terminal"));
        assert!(requires_approval("file_write"));
        assert!(requires_approval("patch"));
        assert!(requires_approval("edit_block"));
        assert!(requires_approval("code_execution"));
    }

    #[test]
    fn read_only_tools_skip_approval() {
        assert!(!requires_approval("file_read"));
        assert!(!requires_approval("web_search"));
        assert!(!requires_approval("web_fetch"));
        assert!(!requires_approval("memory_search"));
        assert!(!requires_approval("todo"));
        assert!(!requires_approval("delegate_to_sub_agent"));
    }

    #[tokio::test]
    async fn decision_round_trip() {
        let (id, rx) = crate::gateway::register_pending_approval();
        assert!(crate::gateway::resolve_pending_approval(id, true));
        assert_eq!(rx.await, Ok(true));
        // Second resolve of the same ID is a no-op (stale button press).
        assert!(!crate::gateway::resolve_pending_approval(id, false));
    }

    #[tokio::test]
    async fn drop_pending_approval_cancels_waiter() {
        let (id, rx) = crate::gateway::register_pending_approval();
        crate::gateway::drop_pending_approval(id);
        // Sender dropped → receiver errors instead of hanging.
        assert!(rx.await.is_err());
    }
}
