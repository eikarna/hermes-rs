//! Edit-protocol outcome measurement (Task 2.4).
//!
//! Whenever an edit-format tool response is applied, kerux journals an
//! `edit_outcome` event capturing which edit protocol was used, whether the
//! arguments parsed, whether the edit applied, the match strategy, target
//! language, model/provider identity, and how many prior failed attempts
//! ("repairs") the target path had within the same run.
//!
//! This module is strictly *measurement*: it never alters edit-format
//! routing or selection logic. Non-edit tools produce no events.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Tools that perform structured file edits, mapped to their edit protocol.
pub const EDIT_TOOL_NAMES: [&str; 3] = ["edit_block", "patch", "file_write"];

/// The edit protocol selected for a file modification attempt.
///
/// Mapping (fixed by the tool registry, not configurable):
/// - `edit_block` → [`EditFormat::SearchReplace`] (Aider-style SEARCH/REPLACE blocks)
/// - `patch` → [`EditFormat::Patch`] (single find/replace)
/// - `file_write` → [`EditFormat::FullFile`] (whole-file rewrite)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    /// Aider-style `<<<<<<< SEARCH` / `=======` / `>>>>>>> REPLACE` blocks.
    SearchReplace,
    /// Single find/replace patch with fuzzy fallback.
    Patch,
    /// Whole-file rewrite.
    FullFile,
}

impl EditFormat {
    /// Map a tool name to its fixed edit protocol. Returns `None` for
    /// non-edit tools so they can be skipped without special-casing.
    pub fn from_tool_name(tool_name: &str) -> Option<Self> {
        match tool_name {
            "edit_block" => Some(Self::SearchReplace),
            "patch" => Some(Self::Patch),
            "file_write" => Some(Self::FullFile),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SearchReplace => "search_replace",
            Self::Patch => "patch",
            Self::FullFile => "full_file",
        }
    }

    /// Convert into the client-layer capability enum (same three variants).
    pub fn into_client(self) -> crate::client::EditFormat {
        match self {
            Self::SearchReplace => crate::client::EditFormat::SearchReplace,
            Self::Patch => crate::client::EditFormat::Patch,
            Self::FullFile => crate::client::EditFormat::FullFile,
        }
    }
}

/// Task 2.5: static, tested fallback order between edit protocols.
///
/// After a *classified* edit-application failure (the edit tool ran and
/// reported it could not apply the edit), the agent may retry with the next
/// protocol in this ladder. Semantic test failures never trigger fallback,
/// and learned/sample-driven routing is deliberately out of scope until
/// enough local outcomes accumulate.
pub struct EditFormatFallback;

impl EditFormatFallback {
    /// Next format to try after `current` failed to apply. `None` means the
    /// ladder is exhausted (a whole-file rewrite is the terminal rung: it
    /// always applies by construction).
    pub fn next_after(current: EditFormat) -> Option<EditFormat> {
        match current {
            EditFormat::SearchReplace => Some(EditFormat::Patch),
            EditFormat::Patch => Some(EditFormat::FullFile),
            EditFormat::FullFile => None,
        }
    }

    /// Position of a format on the ladder; higher = stronger fallback.
    pub fn rank(format: EditFormat) -> u8 {
        match format {
            EditFormat::SearchReplace => 0,
            EditFormat::Patch => 1,
            EditFormat::FullFile => 2,
        }
    }
}

/// How the model's search pattern was matched in the target file.
/// Absent for full-file rewrites and failed parses/applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditMatchType {
    Exact,
    Fuzzy,
}

impl EditMatchType {
    /// Parse the `"exact"`/`"fuzzy"` strings emitted by the edit tools'
    /// success payloads (`matchType` for patch, first entry of `matchTypes`
    /// for edit_block).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "exact" => Some(Self::Exact),
            "fuzzy" => Some(Self::Fuzzy),
            _ => None,
        }
    }
}

/// Terminal state of the apply phase for one edit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditApplyStatus {
    /// Edit applied and written to disk.
    Ok,
    /// Tool ran but returned an error (bad block, missing search text, IO).
    Failed,
    /// Tool reported a timeout at the agent's `tool_timeout`.
    Timeout,
    /// Human approval gate denied the edit before execution.
    Denied,
    /// Never reached execution (invalid argument JSON, unknown tool).
    Skipped,
}

/// Parse status of the model-supplied tool arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditParseStatus {
    Ok,
    Failed,
}

/// Which generation of the run produced an edit outcome (Task 2.3).
///
/// A run starts as a *first pass*: the model edits with whatever context it
/// gathered initially. When an attempt fails and the conversation is repaired
/// with that failure evidence, subsequent attempts are *repair passes*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditPassKind {
    /// Produced during the model's first generation, before any repair.
    FirstPass,
    /// Produced after failure evidence was fed back into the conversation.
    RepairPass,
}

impl EditPassKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FirstPass => "first_pass",
            Self::RepairPass => "repair_pass",
        }
    }
}

/// Default per-path repair budget ([`EditMetricsTracker`]).
///
/// Bounds how many evidence-fed repair rounds are attributed to one target
/// path within a single run before its repair budget reports as exhausted.
pub const DEFAULT_REPAIR_BUDGET: u32 = 3;

/// Per-run edit-attempt tracking (Tasks 2.3 + 2.4).
///
/// Counts failed edit attempts per normalized target path so each
/// `edit_outcome` event can report:
/// - how many prior failures ("repairs") preceded it within the run,
/// - whether it came from the first generation or an evidence-fed repair,
/// - whether the path's bounded repair budget still had room.
///
/// Measurement only: nothing here gates execution.
#[derive(Default)]
pub struct EditMetricsTracker {
    repairs: HashMap<String, u32>,
    exhausted: std::collections::HashSet<String>,
    repair_budget: u32,
    /// 1-based identity of the current top-level run attempt (Task 2.3).
    /// Attempt 1 is the first generation; higher values are evidence-fed
    /// repair attempts driven by the outer self-healing loop.
    run_attempt: u64,
    /// Task 2.5 one-way fallback hint promoted by classified edit failures.
    /// `None` keeps capability-table routing for the current model.
    format_hint: Option<EditFormat>,
}

impl EditMetricsTracker {
    pub fn new() -> Self {
        Self {
            repair_budget: DEFAULT_REPAIR_BUDGET,
            ..Self::default()
        }
    }

    /// Build a tracker with an explicit per-path repair budget (Task 2.3).
    pub fn with_repair_budget(budget: u32) -> Self {
        Self {
            repair_budget: budget,
            ..Self::default()
        }
    }

    /// Set the per-path repair budget (from `[behavior]` config).
    /// `0` disables repair attribution headroom: the first failure on a path
    /// already reports the budget as exhausted.
    pub fn set_repair_budget(&mut self, budget: u32) {
        self.repair_budget = budget;
    }

    pub fn repair_budget(&self) -> u32 {
        self.repair_budget
    }

    /// Tag the current top-level run attempt (called by the outer healing
    /// loop each time it retries the whole run).
    pub fn set_run_attempt(&mut self, attempt: u64) {
        self.run_attempt = attempt.max(1);
    }

    /// Current run attempt identity (defaults to the first attempt).
    pub fn run_attempt(&self) -> u64 {
        if self.run_attempt == 0 {
            1
        } else {
            self.run_attempt
        }
    }

    /// Observe one edit attempt for `path`.
    ///
    /// Returns `(prior_failures, pass_kind, repair_allowed)`:
    /// - `pass_kind` is [`EditPassKind::FirstPass`] until a failure has been
    ///   recorded for the path, then [`EditPassKind::RepairPass`].
    /// - `repair_allowed` reports whether another evidence-fed repair would
    ///   fit the budget after a failure here. Once a path exceeds the budget
    ///   it stays exhausted for the rest of the run, so runaway loops stay
    ///   visible in telemetry instead of silently resetting.
    pub fn observe(&mut self, path: &str, failed: bool) -> (u32, EditPassKind, bool) {
        let prior = self.repairs.get(path).copied().unwrap_or(0);
        let pass_kind = if prior == 0 {
            EditPassKind::FirstPass
        } else {
            EditPassKind::RepairPass
        };
        if failed && prior + 1 > self.repair_budget {
            self.exhausted.insert(path.to_string());
        }
        if failed {
            self.repairs.insert(path.to_string(), prior + 1);
        }
        let repair_allowed = !self.exhausted.contains(path);
        (prior, pass_kind, repair_allowed)
    }

    /// Task 2.5: after a *classified* edit-application failure on `failed`
    /// format for `path`, promote the fallback hint to the next ladder rung.
    ///
    /// Classification mirrors the journaling rule in the agent: apply
    /// failures and parse failures that stopped the edit before execution
    /// qualify; cancels/denials/timeouts and successful edits do not.
    /// One-way: an existing stronger hint always wins, so a later
    /// lower-protocol failure never demotes routing mid-run.
    ///
    /// `path` is accepted for call-site symmetry and future per-path
    /// routing; the current ladder is run-global by design.
    pub fn record_fallback(&mut self, _path: &str, failed: EditFormat) {
        if let Some(next) = EditFormatFallback::next_after(failed) {
            let promote = self.format_hint.is_none_or(|current| {
                EditFormatFallback::rank(current) < EditFormatFallback::rank(next)
            });
            if promote {
                self.format_hint = Some(next);
            }
        }
    }

    /// Current fallback hint (`None` = keep capability-table routing).
    pub fn format_hint(&self) -> Option<EditFormat> {
        self.format_hint
    }

    /// Forget all counters. Called when a recorded run starts so counts
    /// never leak across runs.
    pub fn reset(&mut self) {
        self.repairs.clear();
        self.exhausted.clear();
        self.run_attempt = 0;
        self.format_hint = None;
    }
}

/// Best-effort source-language detection from a target path extension.
/// Returns a lowercase language tag, or `"unknown"` when the extension is
/// unrecognized or absent.
pub fn language_for_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "cs" => "csharp",
        "sh" | "bash" => "shell",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "xml" => "xml",
        "html" | "htm" => "html",
        "css" => "css",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        _ => "unknown",
    }
}

/// Extract the target file path from an edit tool's argument JSON.
///
/// `edit_block` and `file_write` use `path`; `patch` uses `file_path`.
/// Returns `None` when the arguments are not valid JSON or carry no path.
pub fn extract_target_path(args_json: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(args_json).ok()?;
    args.get("path")
        .or_else(|| args.get("file_path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Read the match strategy reported by a successful edit tool payload.
///
/// `edit_block` reports `matchTypes` (array, first entry wins); `patch`
/// reports `matchType` (string). Full-file rewrites report neither.
pub fn match_type_from_payload(content: &str) -> Option<EditMatchType> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let raw = value
        .get("matchTypes")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .or_else(|| value.get("matchType").and_then(|v| v.as_str()))?;
    EditMatchType::parse(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_tool_names_to_formats() {
        assert_eq!(
            EditFormat::from_tool_name("edit_block"),
            Some(EditFormat::SearchReplace)
        );
        assert_eq!(EditFormat::from_tool_name("patch"), Some(EditFormat::Patch));
        assert_eq!(
            EditFormat::from_tool_name("file_write"),
            Some(EditFormat::FullFile)
        );
        assert_eq!(EditFormat::from_tool_name("file_read"), None);
        assert_eq!(EditFormat::from_tool_name(""), None);
    }

    #[test]
    fn formats_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(EditFormat::SearchReplace).unwrap(),
            serde_json::json!("search_replace")
        );
        assert_eq!(
            serde_json::to_value(EditApplyStatus::Timeout).unwrap(),
            serde_json::json!("timeout")
        );
        assert_eq!(
            serde_json::to_value(EditParseStatus::Ok).unwrap(),
            serde_json::json!("ok")
        );
    }

    #[test]
    fn parses_match_types_from_tool_payloads() {
        assert_eq!(EditMatchType::parse("exact"), Some(EditMatchType::Exact));
        assert_eq!(EditMatchType::parse("fuzzy"), Some(EditMatchType::Fuzzy));
        assert_eq!(EditMatchType::parse("regex"), None);
    }

    #[test]
    fn repair_counts_accumulate_per_path_and_ignore_successes() {
        let mut state = EditMetricsTracker::new();
        assert_eq!(state.observe("/a.rs", false).0, 0);
        assert_eq!(state.observe("/a.rs", true).0, 0);
        assert_eq!(state.observe("/a.rs", true).0, 1);
        assert_eq!(state.observe("/a.rs", false).0, 2);
        assert_eq!(state.observe("/b.rs", true).0, 0);
        assert_eq!(state.observe("/a.rs", false).0, 2);
    }

    #[test]
    fn reset_clears_counters() {
        let mut state = EditMetricsTracker::new();
        state.observe("/a.rs", true);
        state.observe("/a.rs", true);
        state.reset();
        assert_eq!(state.observe("/a.rs", false).0, 0);
    }

    #[test]
    fn pass_kind_flips_after_first_failure_per_path() {
        let mut state = EditMetricsTracker::new();
        let (_, kind, allowed) = state.observe("src/lib.rs", false);
        assert_eq!(kind, EditPassKind::FirstPass);
        assert!(allowed);
        // The failing attempt itself was still a first-generation one; only
        // the *next* attempt runs with failure evidence to repair from.
        let (_, kind, allowed) = state.observe("src/lib.rs", true);
        assert_eq!(kind, EditPassKind::FirstPass);
        assert!(allowed); // still within the default budget of 3
        let (_, kind, allowed) = state.observe("src/lib.rs", false);
        assert_eq!(kind, EditPassKind::RepairPass);
        assert!(allowed);
    }

    #[test]
    fn run_attempt_defaults_to_one_and_survives_reset() {
        let mut state = EditMetricsTracker::new();
        assert_eq!(state.run_attempt(), 1);
        state.set_run_attempt(3);
        assert_eq!(state.run_attempt(), 3);
        state.reset();
        assert_eq!(state.run_attempt(), 1);
        state.set_run_attempt(0);
        assert_eq!(state.run_attempt(), 1); // clamped to first attempt
    }

    #[test]
    fn bounded_budget_flags_exhaustion_and_stays_put() {
        let mut state = EditMetricsTracker::with_repair_budget(1);
        assert_eq!(state.repair_budget(), 1);
        let (prior, _, allowed) = state.observe("f.py", true);
        assert_eq!((prior, allowed), (0, true)); // failure #1 fits the budget
        let (prior, kind, allowed) = state.observe("f.py", true);
        assert_eq!((prior, kind), (1, EditPassKind::RepairPass));
        assert!(!allowed); // failure #2 exceeds the budget
        let (_, _, allowed) = state.observe("f.py", false);
        assert!(!allowed); // exhaustion sticks for the rest of the run
        let (_, _, allowed) = state.observe("g.py", true);
        assert!(allowed); // other paths are unaffected
        let mut zero = EditMetricsTracker::with_repair_budget(0);
        let (_, _, allowed) = zero.observe("z.py", true);
        assert!(!allowed); // zero budget: the first failure already exhausts
    }

    #[test]
    fn fallback_ladder_is_static() {
        assert_eq!(
            EditFormatFallback::next_after(EditFormat::SearchReplace),
            Some(EditFormat::Patch)
        );
        assert_eq!(
            EditFormatFallback::next_after(EditFormat::Patch),
            Some(EditFormat::FullFile)
        );
        assert_eq!(EditFormatFallback::next_after(EditFormat::FullFile), None);
        assert!(
            EditFormatFallback::rank(EditFormat::SearchReplace)
                < EditFormatFallback::rank(EditFormat::Patch)
        );
        assert!(
            EditFormatFallback::rank(EditFormat::Patch)
                < EditFormatFallback::rank(EditFormat::FullFile)
        );
    }

    #[test]
    fn fallback_hint_promotes_one_way_and_never_leaks_across_runs() {
        let mut state = EditMetricsTracker::new();
        assert_eq!(state.format_hint(), None);
        state.record_fallback("a.rs", EditFormat::SearchReplace);
        assert_eq!(state.format_hint(), Some(EditFormat::Patch));
        state.record_fallback("b.rs", EditFormat::Patch);
        assert_eq!(state.format_hint(), Some(EditFormat::FullFile));
        // Weaker protocols never demote an existing stronger hint...
        state.record_fallback("c.rs", EditFormat::SearchReplace);
        assert_eq!(state.format_hint(), Some(EditFormat::FullFile));
        // ...and the terminal rung leaves the hint unchanged.
        state.record_fallback("d.rs", EditFormat::FullFile);
        assert_eq!(state.format_hint(), Some(EditFormat::FullFile));
        state.reset();
        assert_eq!(state.format_hint(), None);
    }

    #[test]
    fn detects_languages_from_extensions() {
        assert_eq!(language_for_path("src/main.rs"), "rust");
        assert_eq!(language_for_path("/x/y/script.py"), "python");
        assert_eq!(language_for_path("app.tsx"), "tsx");
        assert_eq!(language_for_path("Cargo.toml"), "toml");
        assert_eq!(language_for_path("notes.weirdext"), "unknown");
        assert_eq!(language_for_path("Makefile"), "unknown");
    }
}
