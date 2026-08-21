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

/// Per-run repair tracking. Counts failed edit attempts per normalized
/// target path so each `edit_outcome` event can report how many prior
/// failures ("repairs") preceded it within the same run.
#[derive(Default)]
pub struct EditMetricsTracker {
    repairs: HashMap<String, u32>,
}

impl EditMetricsTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of prior failed attempts for `path` in this run, then record
    /// `outcome` for future lookups. Successful attempts do not reset the
    /// counter: a later success after two failures reports `repair_count: 2`.
    pub fn observe(&mut self, path: &str, failed: bool) -> u32 {
        let prior = self.repairs.get(path).copied().unwrap_or(0);
        if failed {
            self.repairs.insert(path.to_string(), prior + 1);
        }
        prior
    }

    /// Forget all counters. Called when a recorded run starts so counts
    /// never leak across runs.
    pub fn reset(&mut self) {
        self.repairs.clear();
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
        assert_eq!(state.observe("/a.rs", false), 0);
        assert_eq!(state.observe("/a.rs", true), 0);
        assert_eq!(state.observe("/a.rs", true), 1);
        assert_eq!(state.observe("/a.rs", false), 2);
        assert_eq!(state.observe("/b.rs", true), 0);
        assert_eq!(state.observe("/a.rs", false), 2);
    }

    #[test]
    fn reset_clears_counters() {
        let mut state = EditMetricsTracker::new();
        state.observe("/a.rs", true);
        state.observe("/a.rs", true);
        state.reset();
        assert_eq!(state.observe("/a.rs", false), 0);
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
