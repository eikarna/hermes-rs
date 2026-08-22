//! Deterministic project validators — policy definition (Task 2.1).
//!
//! A [`ValidationPolicy`] is an ordered list of shell-free validator commands
//! (fmt, lint, test, ...) that kerux can run against a workspace to produce
//! machine-checkable evidence. This module only *defines* the policy and its
//! result types; execution and journaling land in Task 2.2.
//!
//! Deliberately minimal in v1: no plugin protocol, no retries, no caching.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Default per-validator timeout in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Default cap for captured stdout+stderr of one validator, in bytes.
pub const DEFAULT_OUTPUT_CAP_BYTES: usize = 16 * 1024;

/// One validator command, executed in declaration order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ValidatorSpec {
    /// Stable human-readable identifier (used in events and results).
    pub name: String,
    /// Command line, split on ASCII whitespace (`argv[0]` is the program).
    /// No shell interpolation — the string is never passed to a shell.
    pub command: String,
    /// Whether a failure of this validator fails the whole validation pass.
    #[serde(default = "default_required")]
    pub required: bool,
    /// Per-validator timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Working directory relative to the workspace root (`None` = root).
    /// Must be relative and must not escape the workspace.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Maximum bytes of captured stdout+stderr kept for evidence.
    #[serde(default = "default_output_cap_bytes")]
    pub output_cap_bytes: usize,
}

fn default_required() -> bool {
    true
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

fn default_output_cap_bytes() -> usize {
    DEFAULT_OUTPUT_CAP_BYTES
}

impl ValidatorSpec {
    /// Split `command` into argv (ASCII-whitespace separated, no shell).
    pub fn argv(&self) -> Vec<&str> {
        self.command.split_ascii_whitespace().collect()
    }

    /// Resolve `workdir` against the workspace root.
    ///
    /// Returns the workspace root when `workdir` is `None`.
    pub fn resolved_workdir(&self, workspace: &Path) -> PathBuf {
        match &self.workdir {
            None => workspace.to_path_buf(),
            Some(relative) => workspace.join(relative),
        }
    }

    /// Structural checks for one spec (used at config-load time).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Config(
                "validator name must not be empty".to_string(),
            ));
        }
        if self.argv().is_empty() {
            return Err(Error::Config(format!(
                "validator '{}' has an empty command",
                self.name
            )));
        }
        if self.timeout_secs == 0 {
            return Err(Error::Config(format!(
                "validator '{}' timeout_secs must be > 0",
                self.name
            )));
        }
        if self.output_cap_bytes == 0 {
            return Err(Error::Config(format!(
                "validator '{}' output_cap_bytes must be > 0",
                self.name
            )));
        }
        if let Some(workdir) = &self.workdir {
            let path = Path::new(workdir);
            if path
                .components()
                .any(|component| matches!(component, Component::RootDir | Component::Prefix(_)))
            {
                return Err(Error::Config(format!(
                    "validator '{}' workdir must be relative to the workspace",
                    self.name
                )));
            }
            if path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(Error::Config(format!(
                    "validator '{}' workdir must not escape the workspace ('..')",
                    self.name
                )));
            }
        }
        Ok(())
    }
}

/// Ordered, deterministic validation policy for one workspace.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ValidationPolicy {
    /// Whether validation passes run at all.
    pub enabled: bool,
    /// Validators in execution order.
    pub validators: Vec<ValidatorSpec>,
    /// Stop at the first *required* failure instead of running the rest.
    #[serde(default)]
    pub fail_fast: bool,
}

impl ValidationPolicy {
    /// Structural checks for the whole policy (used at config-load time).
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for spec in &self.validators {
            spec.validate()?;
            if !seen.insert(spec.name.as_str()) {
                return Err(Error::Config(format!(
                    "duplicate validator name '{}'",
                    spec.name
                )));
            }
        }
        Ok(())
    }

    /// Serialize this policy into a journal-friendly manifest value.
    pub fn to_policy_value(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "fail_fast": self.fail_fast,
            "validators": self
                .validators
                .iter()
                .map(|spec| {
                    serde_json::json!({
                        "name": spec.name,
                        "required": spec.required,
                        "timeout_secs": spec.timeout_secs,
                    })
                })
                .collect::<Vec<_>>(),
        })
    }
}

/// Outcome of executing one validator (Task 2.2 fills these in).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidatorOutcome {
    /// Exit code 0.
    Passed,
    /// Non-zero exit code.
    Failed,
    /// Killed after `timeout_secs`.
    TimedOut,
    /// Could not spawn the process.
    SpawnError,
    /// Not run (e.g. skipped after a required failure with `fail_fast`).
    Skipped,
}

impl ValidatorOutcome {
    /// Stable string form for journal payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            ValidatorOutcome::Passed => "passed",
            ValidatorOutcome::Failed => "failed",
            ValidatorOutcome::TimedOut => "timed_out",
            ValidatorOutcome::SpawnError => "spawn_error",
            ValidatorOutcome::Skipped => "skipped",
        }
    }
}

/// Result of running one validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorResult {
    pub name: String,
    pub outcome: ValidatorOutcome,
    /// Process exit code (`None` for timeout/spawn error/skipped).
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// Captured stdout+stderr, bounded by `output_cap_bytes`.
    pub output: String,
    /// Whether `output` was truncated to the cap.
    pub output_truncated: bool,
}

impl ValidatorResult {
    /// Whether this result satisfies the policy for its validator.
    pub fn satisfies(&self, required: bool) -> bool {
        !required || self.outcome == ValidatorOutcome::Passed
    }
}

/// Aggregate result of one validation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationPassResult {
    pub results: Vec<ValidatorResult>,
    /// `true` when every *required* validator passed.
    pub passed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, command: &str) -> ValidatorSpec {
        ValidatorSpec {
            name: name.to_string(),
            command: command.to_string(),
            required: true,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            workdir: None,
            output_cap_bytes: DEFAULT_OUTPUT_CAP_BYTES,
        }
    }

    #[test]
    fn defaults_are_disabled_and_empty() {
        let policy: ValidationPolicy = toml::from_str("").unwrap();
        assert!(!policy.enabled);
        assert!(policy.validators.is_empty());
        assert!(!policy.fail_fast);
        policy.validate().unwrap();
    }

    #[test]
    fn toml_parses_ordered_validators_with_defaults() {
        let policy: ValidationPolicy = toml::from_str(
            r#"
            enabled = true
            fail_fast = true

            [[validators]]
            name = "fmt"
            command = "cargo fmt --all -- --check"

            [[validators]]
            name = "test"
            command = "cargo test --workspace"
            required = false
            timeout_secs = 600
            workdir = "crates/kerux-core"
            output_cap_bytes = 4096
            "#,
        )
        .unwrap();
        assert!(policy.enabled);
        assert!(policy.fail_fast);
        assert_eq!(policy.validators.len(), 2);

        let fmt = &policy.validators[0];
        assert!(fmt.required);
        assert_eq!(fmt.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(fmt.output_cap_bytes, DEFAULT_OUTPUT_CAP_BYTES);
        assert_eq!(fmt.workdir, None);

        let test = &policy.validators[1];
        assert!(!test.required);
        assert_eq!(test.timeout_secs, 600);
        assert_eq!(test.workdir.as_deref(), Some("crates/kerux-core"));
        assert_eq!(test.output_cap_bytes, 4096);

        policy.validate().unwrap();
    }

    #[test]
    fn argv_splits_on_whitespace_without_shell() {
        let s = spec("lint", "cargo  clippy   --workspace -- -D warnings");
        assert_eq!(
            s.argv(),
            vec!["cargo", "clippy", "--workspace", "--", "-D", "warnings"]
        );
    }

    #[test]
    fn resolved_workdir_stays_under_workspace() {
        let workspace = Path::new("/tmp/ws");
        let root = spec("a", "true");
        assert_eq!(root.resolved_workdir(workspace), PathBuf::from("/tmp/ws"));
        let nested = ValidatorSpec {
            workdir: Some("crates/core".to_string()),
            ..spec("b", "true")
        };
        assert_eq!(
            nested.resolved_workdir(workspace),
            PathBuf::from("/tmp/ws/crates/core")
        );
    }

    #[test]
    fn empty_command_is_rejected() {
        let s = spec("bad", "   ");
        let error = s.validate().unwrap_err();
        assert!(error.to_string().contains("empty command"));
    }

    #[test]
    fn zero_timeout_and_zero_cap_are_rejected() {
        let timeout = ValidatorSpec {
            timeout_secs: 0,
            ..spec("bad", "true")
        };
        assert!(timeout.validate().is_err());
        let cap = ValidatorSpec {
            output_cap_bytes: 0,
            ..spec("bad", "true")
        };
        assert!(cap.validate().is_err());
    }

    #[test]
    fn absolute_and_escaping_workdir_are_rejected() {
        let absolute = ValidatorSpec {
            workdir: Some("/etc".to_string()),
            ..spec("bad", "true")
        };
        assert!(absolute.validate().is_err());
        let escaping = ValidatorSpec {
            workdir: Some("../outside".to_string()),
            ..spec("bad", "true")
        };
        assert!(escaping.validate().is_err());
    }

    #[test]
    fn duplicate_validator_names_are_rejected() {
        let policy = ValidationPolicy {
            enabled: true,
            validators: vec![spec("fmt", "cargo fmt"), spec("fmt", "cargo fmt")],
            fail_fast: false,
        };
        let error = policy.validate().unwrap_err();
        assert!(error.to_string().contains("duplicate validator name"));
    }

    #[test]
    fn policy_value_is_journal_friendly() {
        let policy = ValidationPolicy {
            enabled: true,
            validators: vec![spec("fmt", "cargo fmt")],
            fail_fast: true,
        };
        let value = policy.to_policy_value();
        assert_eq!(value["enabled"], serde_json::json!(true));
        assert_eq!(value["fail_fast"], serde_json::json!(true));
        assert_eq!(value["validators"][0]["name"], serde_json::json!("fmt"));
        assert_eq!(value["validators"][0]["required"], serde_json::json!(true));
    }

    #[test]
    fn outcome_strings_are_stable() {
        assert_eq!(ValidatorOutcome::Passed.as_str(), "passed");
        assert_eq!(ValidatorOutcome::Failed.as_str(), "failed");
        assert_eq!(ValidatorOutcome::TimedOut.as_str(), "timed_out");
        assert_eq!(ValidatorOutcome::SpawnError.as_str(), "spawn_error");
        assert_eq!(ValidatorOutcome::Skipped.as_str(), "skipped");
    }

    #[test]
    fn satisfies_respects_required_flag() {
        let failed = ValidatorResult {
            name: "lint".to_string(),
            outcome: ValidatorOutcome::Failed,
            exit_code: Some(1),
            duration_ms: 10,
            output: String::new(),
            output_truncated: false,
        };
        assert!(!failed.satisfies(true));
        assert!(failed.satisfies(false));
        let skipped = ValidatorResult {
            outcome: ValidatorOutcome::Skipped,
            exit_code: None,
            ..failed.clone()
        };
        assert!(!skipped.satisfies(true));
        assert!(skipped.satisfies(false));
    }
}
