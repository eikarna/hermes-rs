//! Tool approval gate and pattern-based allow rules.
//!
//! Lets a gateway (or any host) require explicit human approval before the
//! agent executes dangerous tools. The agent consults the gate right before
//! running each tool in [`crate::agent::KeruxAgent`]'s execute loop; the
//! gate is responsible for presenting the request (e.g. a Telegram inline
//! keyboard) and resolving it to a decision.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};

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

/// Human choices available on an interactive approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalChoice {
    /// Allow this specific tool execution once.
    AllowOnce,
    /// Allow this pattern for the duration of the current session.
    Session,
    /// Persist this allow pattern across restarts (~/.kerux/approvals/rules.json).
    AlwaysAllow,
    /// Deny tool execution.
    Reject,
}

/// Why an approval request resolved the way it did.
///
/// Recorded alongside the decision so a run journal can distinguish a human
/// "no" from an auto-deny (timeout), a dropped/cancelled waiter, or a prompt
/// that never reached the human. Approval is a human decision channel — it is
/// never labelled as sandbox enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ApprovalOutcome {
    /// The human explicitly approved the tool call.
    Approved,
    /// The human explicitly denied the tool call.
    Denied,
    /// No decision arrived before the gate's timeout (auto-deny).
    Timeout,
    /// The decision channel closed without a decision (stale or cancelled).
    ChannelClosed,
    /// The approval prompt could not be delivered to the human.
    PromptFailed,
}

/// Outcome of an approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run the tool.
    Approved,
    /// Do not run the tool; feed `reason` back to the model as the tool error.
    /// `outcome` records *why* it was denied (human, timeout, cancelled, ...).
    Denied {
        reason: String,
        outcome: ApprovalOutcome,
    },
}

/// Persistent approval rule matching tool name and argument regex pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRule {
    /// Tool name to match (e.g. "terminal", "file_write", or "*" for any).
    pub tool_name: String,
    /// Regex pattern to match against `arguments_preview` or full args string.
    pub pattern: String,
    /// Optional description or timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// In-memory and on-disk registry of pattern-based approval rules.
pub struct ApprovalRuleStore {
    persist_path: Option<PathBuf>,
    persistent_rules: RwLock<Vec<(ApprovalRule, Regex)>>,
    session_rules: RwLock<HashMap<String, Vec<(ApprovalRule, Regex)>>>,
}

impl ApprovalRuleStore {
    /// Create a new rule store, loading persistent rules from default path if available.
    pub fn new(persist_path: Option<PathBuf>) -> Self {
        let store = Self {
            persist_path,
            persistent_rules: RwLock::new(Vec::new()),
            session_rules: RwLock::new(HashMap::new()),
        };
        store.load_persistent();
        store
    }

    /// Default persistent rules file location: `~/.kerux/approvals/rules.json`.
    pub fn default_path() -> PathBuf {
        crate::persist::data_dir("approvals").join("rules.json")
    }

    /// Load persistent rules from disk.
    fn load_persistent(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        if let Some(rules) = crate::persist::read_json::<Vec<ApprovalRule>>(path) {
            let mut compiled = Vec::new();
            for r in rules {
                if let Ok(re) = Regex::new(&r.pattern) {
                    compiled.push((r, re));
                }
            }
            if let Ok(mut lock) = self.persistent_rules.write() {
                *lock = compiled;
            }
        }
    }

    /// Save persistent rules to disk.
    fn save_persistent(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let rules: Vec<ApprovalRule> = match self.persistent_rules.read() {
            Ok(lock) => lock.iter().map(|(r, _)| r.clone()).collect(),
            Err(_) => return,
        };
        let _ = crate::persist::write_json(path, &rules);
    }

    /// Check if a tool call with the given args preview is allowed by persistent or session rules.
    pub fn is_allowed(&self, session_key: Option<&str>, tool_name: &str, args_preview: &str) -> bool {
        // 1. Check persistent rules
        if let Ok(rules) = self.persistent_rules.read() {
            for (rule, re) in rules.iter() {
                if (rule.tool_name == "*" || rule.tool_name.eq_ignore_ascii_case(tool_name))
                    && re.is_match(args_preview)
                {
                    return true;
                }
            }
        }

        // 2. Check session rules
        if let Some(key) = session_key {
            if let Ok(sessions) = self.session_rules.read() {
                if let Some(rules) = sessions.get(key) {
                    for (rule, re) in rules.iter() {
                        if (rule.tool_name == "*" || rule.tool_name.eq_ignore_ascii_case(tool_name))
                            && re.is_match(args_preview)
                        {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }

    /// Add a session-scoped allow rule.
    pub fn add_session_rule(&self, session_key: &str, tool_name: &str, pattern: &str) -> bool {
        let re = match Regex::new(pattern) {
            Ok(re) => re,
            Err(_) => return false,
        };
        let rule = ApprovalRule {
            tool_name: tool_name.to_string(),
            pattern: pattern.to_string(),
            comment: Some("Session allow".to_string()),
        };
        if let Ok(mut sessions) = self.session_rules.write() {
            sessions
                .entry(session_key.to_string())
                .or_default()
                .push((rule, re));
            true
        } else {
            false
        }
    }

    /// Add a persistent (Always Allow) rule and save to disk.
    pub fn add_persistent_rule(&self, tool_name: &str, pattern: &str) -> bool {
        let re = match Regex::new(pattern) {
            Ok(re) => re,
            Err(_) => return false,
        };
        let rule = ApprovalRule {
            tool_name: tool_name.to_string(),
            pattern: pattern.to_string(),
            comment: Some("Always allow".to_string()),
        };
        if let Ok(mut lock) = self.persistent_rules.write() {
            lock.push((rule, re));
            drop(lock);
            self.save_persistent();
            true
        } else {
            false
        }
    }
}

/// Global shared rule store instance.
static GLOBAL_RULE_STORE: std::sync::LazyLock<Arc<ApprovalRuleStore>> =
    std::sync::LazyLock::new(|| Arc::new(ApprovalRuleStore::new(Some(ApprovalRuleStore::default_path()))));

/// Access the global approval rule store.
pub fn global_rule_store() -> Arc<ApprovalRuleStore> {
    GLOBAL_RULE_STORE.clone()
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

    #[test]
    fn rule_store_matching() {
        let store = ApprovalRuleStore::new(None);
        assert!(!store.is_allowed(Some("session1"), "terminal", "git status"));

        // Add session rule
        assert!(store.add_session_rule("session1", "terminal", r"^git\s+.*"));
        assert!(store.is_allowed(Some("session1"), "terminal", "git status"));
        assert!(store.is_allowed(Some("session1"), "terminal", "git log -n 5"));
        assert!(!store.is_allowed(Some("session2"), "terminal", "git status"));
        assert!(!store.is_allowed(Some("session1"), "file_write", "git status"));

        // Add persistent rule
        assert!(store.add_persistent_rule("file_write", r".*\.log$"));
        assert!(store.is_allowed(Some("session2"), "file_write", "app.log"));
        assert!(!store.is_allowed(Some("session2"), "file_write", "main.rs"));
    }

    #[tokio::test]
    async fn decision_round_trip() {
        let (id, rx) = crate::gateway::register_pending_approval("terminal", "echo test");
        assert!(crate::gateway::resolve_pending_approval(id, ApprovalChoice::AllowOnce, None));
        assert_eq!(rx.await, Ok(ApprovalChoice::AllowOnce));
        // Second resolve of the same ID is a no-op (stale button press).
        assert!(!crate::gateway::resolve_pending_approval(id, ApprovalChoice::Reject, None));
    }

    #[tokio::test]
    async fn drop_pending_approval_cancels_waiter() {
        let (id, rx) = crate::gateway::register_pending_approval("terminal", "echo test");
        crate::gateway::drop_pending_approval(id);
        // Sender dropped → receiver errors instead of hanging.
        assert!(rx.await.is_err());
    }
}
