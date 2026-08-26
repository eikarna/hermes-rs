//! Preference extraction from trajectory history.
//!
//! Implements the [`PreferenceExtractor`] contract: parse recorded
//! [`Trajectory`] steps and emit [`PreferenceObservation`]s that fold into a
//! [`TasteProfile`](crate::taste::TasteProfile) via
//! [`TasteProfile::apply_observations`](crate::taste::TasteProfile::apply_observations).
//! Confidence scoring stays in [`crate::taste`] — this module only emits
//! evidence.
//!
//! ## Design decisions
//!
//! - **Steps only.** The canonical action record of a trajectory is its
//!   `steps` (`action` + `action_args` JSON). `messages` mirror the same tool
//!   calls, so mining both would double-count evidence.
//! - **Behavioral, positive evidence.** Observations are `supports = true`
//!   records of what the work actually exhibited. Counter-evidence
//!   (`supports = false`) is reserved for explicit/manual signals; guessing
//!   contradictions from history is too noisy.
//! - **Deterministic.** No LLM involvement: token matching on terminal
//!   commands, extension/identifier heuristics on file paths, and whitespace
//!   analysis on written content.
//! - **Repeat cap.** A tight retry loop running the same command must not
//!   saturate a preference from a single session, so identical
//!   `(key, value)` observations are capped per trajectory
//!   ([`TrajectoryPreferenceExtractor::max_repeats_per_trajectory`]).
//! - **Stable keys.** Emitted preference keys: `test runner`, `linter`,
//!   `formatter`, `build tool`, `primary language`, `file naming`,
//!   `indentation`, `edit style`, `commit style`, `test discipline`.
//!
//! ## Assumptions
//!
//! `action_args` is the JSON document the tool registry deserializes
//! (camelCase keys): `terminal` → `{"command": ..}`, `file_write` →
//! `{"path": .., "content": .., "append": ..}`, `patch` →
//! `{"path": .., "find": .., "replace": ..}`, `edit_block` →
//! `{"path": .., "edits": [{"search": .., "replace": ..}]}`.

use std::collections::HashMap;

use serde_json::Value;

use crate::taste::{
    PreferenceCategory, PreferenceExtractor, PreferenceObservation, PreferenceSource,
};
use crate::trajectory::{Trajectory, TrajectoryStep};

/// Default cap on identical `(key, value)` observations per trajectory.
pub const DEFAULT_MAX_REPEATS: usize = 3;

/// Mines coding-style evidence from recorded trajectories.
///
/// Cheap and allocation-light: safe to run over the full trajectory history
/// before folding into a profile.
#[derive(Debug, Clone)]
pub struct TrajectoryPreferenceExtractor {
    /// Maximum observations with the same `(key, value)` emitted for one
    /// trajectory. Guards against retry loops inflating a single session.
    /// `0` disables emission entirely.
    pub max_repeats_per_trajectory: usize,
}

impl Default for TrajectoryPreferenceExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl TrajectoryPreferenceExtractor {
    /// Extractor with the default repeat cap ([`DEFAULT_MAX_REPEATS`]).
    pub fn new() -> Self {
        Self {
            max_repeats_per_trajectory: DEFAULT_MAX_REPEATS,
        }
    }

    fn extract_step(
        &self,
        step: &TrajectoryStep,
        observed_at: i64,
        edit_pending: &mut bool,
        counts: &mut HashMap<(String, String), usize>,
        observations: &mut Vec<PreferenceObservation>,
    ) {
        let Some(action) = step.action.as_deref() else {
            return;
        };
        let args = parse_args(step.action_args.as_deref());
        match action {
            "terminal" => {
                let Some(command) = args.get("command").and_then(Value::as_str) else {
                    return;
                };
                for segment in split_commands(command) {
                    let tokens: Vec<&str> = segment.split_whitespace().collect();
                    if let Some((key, category, value)) = classify_command(&tokens) {
                        if key == "test runner" && *edit_pending {
                            emit(
                                observations,
                                counts,
                                self.max_repeats_per_trajectory,
                                (
                                    "test discipline",
                                    PreferenceCategory::Testing,
                                    "runs tests after edits",
                                ),
                                observed_at,
                            );
                            *edit_pending = false;
                        }
                        emit(
                            observations,
                            counts,
                            self.max_repeats_per_trajectory,
                            (key, category, value),
                            observed_at,
                        );
                    }
                    if let Some(style) = classify_git_commit(&tokens) {
                        emit(
                            observations,
                            counts,
                            self.max_repeats_per_trajectory,
                            ("commit style", PreferenceCategory::Workflow, style),
                            observed_at,
                        );
                    }
                }
            }
            "file_write" => {
                let Some(path) = args.get("path").and_then(Value::as_str) else {
                    return;
                };
                let append = args.get("append").and_then(Value::as_bool).unwrap_or(false);
                self.path_signals(path, counts, observed_at, observations);
                if let Some(content) = args.get("content").and_then(Value::as_str) {
                    if let Some(indent) = detect_indentation(content) {
                        emit(
                            observations,
                            counts,
                            self.max_repeats_per_trajectory,
                            ("indentation", PreferenceCategory::Formatting, indent),
                            observed_at,
                        );
                    }
                }
                if !append {
                    emit(
                        observations,
                        counts,
                        self.max_repeats_per_trajectory,
                        (
                            "edit style",
                            PreferenceCategory::Architecture,
                            "full file rewrites",
                        ),
                        observed_at,
                    );
                }
                *edit_pending = true;
            }
            "patch" => {
                let Some(path) = args.get("path").and_then(Value::as_str) else {
                    return;
                };
                self.path_signals(path, counts, observed_at, observations);
                if let Some(replace) = args.get("replace").and_then(Value::as_str) {
                    if let Some(indent) = detect_indentation(replace) {
                        emit(
                            observations,
                            counts,
                            self.max_repeats_per_trajectory,
                            ("indentation", PreferenceCategory::Formatting, indent),
                            observed_at,
                        );
                    }
                }
                emit(
                    observations,
                    counts,
                    self.max_repeats_per_trajectory,
                    (
                        "edit style",
                        PreferenceCategory::Architecture,
                        "targeted patches",
                    ),
                    observed_at,
                );
                *edit_pending = true;
            }
            "edit_block" => {
                let Some(path) = args.get("path").and_then(Value::as_str) else {
                    return;
                };
                self.path_signals(path, counts, observed_at, observations);
                if let Some(edits) = args.get("edits").and_then(Value::as_array) {
                    let joined: String = edits
                        .iter()
                        .filter_map(|edit| edit.get("replace").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Some(indent) = detect_indentation(&joined) {
                        emit(
                            observations,
                            counts,
                            self.max_repeats_per_trajectory,
                            ("indentation", PreferenceCategory::Formatting, indent),
                            observed_at,
                        );
                    }
                }
                emit(
                    observations,
                    counts,
                    self.max_repeats_per_trajectory,
                    (
                        "edit style",
                        PreferenceCategory::Architecture,
                        "targeted patches",
                    ),
                    observed_at,
                );
                *edit_pending = true;
            }
            _ => {}
        }
    }

    /// Language and file-naming signals shared by all file-editing tools.
    fn path_signals(
        &self,
        path: &str,
        counts: &mut HashMap<(String, String), usize>,
        observed_at: i64,
        observations: &mut Vec<PreferenceObservation>,
    ) {
        if let Some(language) = language_from_path(path) {
            emit(
                observations,
                counts,
                self.max_repeats_per_trajectory,
                ("primary language", PreferenceCategory::Language, language),
                observed_at,
            );
        }
        if let Some(naming) = file_naming(path) {
            emit(
                observations,
                counts,
                self.max_repeats_per_trajectory,
                ("file naming", PreferenceCategory::Naming, naming),
                observed_at,
            );
        }
    }
}

impl PreferenceExtractor for TrajectoryPreferenceExtractor {
    fn extract(&self, trajectories: &[Trajectory]) -> Vec<PreferenceObservation> {
        let mut observations = Vec::new();
        for trajectory in trajectories {
            let mut counts: HashMap<(String, String), usize> = HashMap::new();
            let mut edit_pending = false;
            for step in &trajectory.steps {
                self.extract_step(
                    step,
                    trajectory.timestamp,
                    &mut edit_pending,
                    &mut counts,
                    &mut observations,
                );
            }
        }
        observations
    }
}

/// Parse `action_args` JSON; malformed or missing arguments become `Null`
/// so downstream `get` calls simply find nothing.
fn parse_args(raw: Option<&str>) -> Value {
    raw.and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null)
}

/// Split a shell one-liner into command segments on `&&`, `||`, `;`, and `|`.
///
/// Quote-unaware by design: this is a style-evidence heuristic, not a shell
/// parser. False segments tokenize to nothing and are ignored.
fn split_commands(command: &str) -> Vec<String> {
    command
        .split(['&', '|', ';'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// Classify one command segment into a tooling preference.
///
/// Two-token patterns win over single-token ones so `cargo test` is never
/// shadowed by a stray tool name elsewhere in the segment.
fn classify_command(tokens: &[&str]) -> Option<(&'static str, PreferenceCategory, &'static str)> {
    use PreferenceCategory::Tooling;
    for pair in tokens.windows(2) {
        match (pair[0], pair[1]) {
            ("cargo", "test") => return Some(("test runner", Tooling, "cargo test")),
            ("cargo", "clippy") => return Some(("linter", Tooling, "clippy")),
            ("cargo", "fmt") => return Some(("formatter", Tooling, "cargo fmt")),
            ("cargo", "build") | ("cargo", "check") => {
                return Some(("build tool", Tooling, "cargo"))
            }
            ("go", "test") => return Some(("test runner", Tooling, "go test")),
            ("go", "build") => return Some(("build tool", Tooling, "go build")),
            ("npm", "test") => return Some(("test runner", Tooling, "npm test")),
            _ => {}
        }
    }
    for token in tokens {
        match *token {
            "pytest" => return Some(("test runner", PreferenceCategory::Tooling, "pytest")),
            "jest" => return Some(("test runner", PreferenceCategory::Tooling, "jest")),
            "vitest" => return Some(("test runner", PreferenceCategory::Tooling, "vitest")),
            "eslint" => return Some(("linter", PreferenceCategory::Tooling, "eslint")),
            "ruff" => return Some(("linter", PreferenceCategory::Tooling, "ruff")),
            "black" => return Some(("formatter", PreferenceCategory::Tooling, "black")),
            "prettier" => return Some(("formatter", PreferenceCategory::Tooling, "prettier")),
            "gofmt" => return Some(("formatter", PreferenceCategory::Tooling, "gofmt")),
            "tsc" => return Some(("build tool", PreferenceCategory::Tooling, "tsc")),
            "make" => return Some(("build tool", PreferenceCategory::Tooling, "make")),
            _ => {}
        }
    }
    None
}

/// Classify git commit staging style from a command segment.
///
/// `git add <path>` (explicit paths) reads as staged, per-file commits;
/// `git add -A|--all|.` and `git commit -a|-am` read as bulk commits.
/// Returns the preference value, or `None` when no commit staging is visible.
fn classify_git_commit(tokens: &[&str]) -> Option<&'static str> {
    for (index, token) in tokens.iter().enumerate() {
        if *token != "git" {
            continue;
        }
        match tokens.get(index + 1).copied() {
            Some("add") => match tokens.get(index + 2).copied() {
                Some("-A") | Some("--all") | Some(".") => return Some("bulk commits"),
                Some(arg) if !arg.starts_with('-') => return Some("staged per-file commits"),
                _ => {}
            },
            Some("commit") => {
                let bulk = tokens[index + 2..]
                    .iter()
                    .any(|flag| *flag == "-a" || *flag == "-am" || *flag == "-ma");
                if bulk {
                    return Some("bulk commits");
                }
            }
            _ => {}
        }
    }
    None
}

/// Map a file extension to a language name; config/doc formats return `None`.
fn language_from_path(path: &str) -> Option<&'static str> {
    let extension = std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some("Rust"),
        "py" => Some("Python"),
        "ts" | "tsx" => Some("TypeScript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "c" | "h" => Some("C"),
        "cpp" | "cc" | "hpp" => Some("C++"),
        "cs" => Some("C#"),
        "rb" => Some("Ruby"),
        "php" => Some("PHP"),
        "swift" => Some("Swift"),
        "kt" => Some("Kotlin"),
        "sh" | "bash" => Some("Shell"),
        _ => None,
    }
}

/// File-name casing convention from the path's file stem.
fn file_naming(path: &str) -> Option<&'static str> {
    let stem = std::path::Path::new(path).file_stem()?.to_str()?;
    classify_identifier(stem)
}

/// Classify an identifier/name into a casing convention.
///
/// Single lowercase words (`mod`, `main`) carry no signal and return `None`,
/// as do mixed forms like `FOO_BAR` or `Foo_Bar`.
fn classify_identifier(name: &str) -> Option<&'static str> {
    if name.len() < 2 {
        return None;
    }
    let has_upper = name.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = name.chars().any(|c| c.is_ascii_lowercase());
    if name.contains('_') {
        let clean = name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        return (clean && has_lower).then_some("snake_case");
    }
    if name.contains('-') {
        let clean = name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        return (clean && has_lower).then_some("kebab-case");
    }
    if has_upper && has_lower {
        let first = name.chars().next()?;
        return if first.is_ascii_uppercase() {
            Some("PascalCase")
        } else {
            Some("camelCase")
        };
    }
    None
}

/// Detect the dominant indentation style of a code sample.
///
/// Uses the shallowest space indent as the base unit (`2`/`3`/`4` spaces);
/// deeper-only samples (min `8`) are ambiguous and return `None`, as do
/// single-space indents. Tab-leading lines win when they outnumber
/// space-indented lines.
fn detect_indentation(code: &str) -> Option<&'static str> {
    let mut tab_lines = 0usize;
    let mut space_indents: Vec<usize> = Vec::new();
    for line in code.lines().take(400) {
        if line.starts_with('\t') {
            tab_lines += 1;
            continue;
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        if spaces > 0 {
            space_indents.push(spaces);
        }
    }
    if space_indents.is_empty() {
        return (tab_lines > 0).then_some("tabs");
    }
    if tab_lines > space_indents.len() {
        return Some("tabs");
    }
    let shallowest = *space_indents.iter().min()?;
    match shallowest {
        2 => Some("2 spaces"),
        3 => Some("3 spaces"),
        4 => Some("4 spaces"),
        _ => None,
    }
}

/// Append one observation unless the per-trajectory repeat cap for its
/// `(key, value)` pair is already reached.
fn emit(
    observations: &mut Vec<PreferenceObservation>,
    counts: &mut HashMap<(String, String), usize>,
    cap: usize,
    signal: (&str, PreferenceCategory, &str),
    observed_at: i64,
) {
    let (key, category, value) = signal;
    let count = counts
        .entry((key.to_string(), value.to_string()))
        .or_insert(0);
    if *count >= cap {
        return;
    }
    *count += 1;
    observations.push(PreferenceObservation {
        key: key.to_string(),
        category,
        value: value.to_string(),
        supports: true,
        weight: 1,
        source: PreferenceSource::Extracted,
        observed_at,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taste::{compute_confidence, TasteProfile};
    use serde_json::json;

    fn step(action: &str, args: Value) -> TrajectoryStep {
        TrajectoryStep {
            step: 0,
            thought: None,
            action: Some(action.to_string()),
            action_args: Some(serde_json::to_string(&args).unwrap()),
            observation: None,
            response: None,
            success: true,
        }
    }

    fn term_step(command: &str) -> TrajectoryStep {
        step("terminal", json!({ "command": command }))
    }

    fn write_step(path: &str, content: &str) -> TrajectoryStep {
        step("file_write", json!({ "path": path, "content": content }))
    }

    fn patch_step(path: &str, find: &str, replace: &str) -> TrajectoryStep {
        step(
            "patch",
            json!({ "path": path, "find": find, "replace": replace }),
        )
    }

    fn edit_block_step(path: &str, edits: Value) -> TrajectoryStep {
        step("edit_block", json!({ "path": path, "edits": edits }))
    }

    fn trajectory(steps: Vec<TrajectoryStep>) -> Trajectory {
        let mut trajectory = Trajectory::new("traj_test", "session", "model");
        trajectory.timestamp = 1_700_000_000;
        trajectory.steps = steps;
        trajectory
    }

    fn extract(steps: Vec<TrajectoryStep>) -> Vec<PreferenceObservation> {
        TrajectoryPreferenceExtractor::new().extract(&[trajectory(steps)])
    }

    fn count_key(observations: &[PreferenceObservation], key: &str) -> usize {
        observations.iter().filter(|o| o.key == key).count()
    }

    fn find<'a>(
        observations: &'a [PreferenceObservation],
        key: &str,
    ) -> Vec<&'a PreferenceObservation> {
        observations.iter().filter(|o| o.key == key).collect()
    }

    #[test]
    fn cargo_test_emits_test_runner() {
        let observations = extract(vec![term_step("cargo test --workspace")]);
        assert_eq!(observations.len(), 1);
        let obs = &observations[0];
        assert_eq!(obs.key, "test runner");
        assert_eq!(obs.value, "cargo test");
        assert_eq!(obs.category, PreferenceCategory::Tooling);
        assert!(obs.supports);
        assert_eq!(obs.weight, 1);
        assert_eq!(obs.source, PreferenceSource::Extracted);
    }

    #[test]
    fn cargo_toolchain_emits_linter_formatter_build_tool() {
        let observations = extract(vec![
            term_step("cargo clippy --workspace -- -D warnings"),
            term_step("cargo fmt --all"),
            term_step("cargo build --release"),
        ]);
        let linter = find(&observations, "linter");
        assert_eq!(linter.len(), 1);
        assert_eq!(linter[0].value, "clippy");
        let formatter = find(&observations, "formatter");
        assert_eq!(formatter.len(), 1);
        assert_eq!(formatter[0].value, "cargo fmt");
        let build = find(&observations, "build tool");
        assert_eq!(build.len(), 1);
        assert_eq!(build[0].value, "cargo");
    }

    #[test]
    fn js_python_go_toolchains_classified() {
        let observations = extract(vec![
            term_step("python -m pytest -q"),
            term_step("npx jest --watch"),
            term_step("eslint src/"),
            term_step("prettier --write ."),
            term_step("go test ./..."),
            term_step("make build"),
            term_step("npx tsc --noEmit"),
        ]);
        let runners: Vec<&str> = find(&observations, "test runner")
            .iter()
            .map(|o| o.value.as_str())
            .collect();
        assert!(runners.contains(&"pytest"));
        assert!(runners.contains(&"jest"));
        assert!(runners.contains(&"go test"));
        let linters: Vec<&str> = find(&observations, "linter")
            .iter()
            .map(|o| o.value.as_str())
            .collect();
        assert!(linters.contains(&"eslint"));
        let formatters: Vec<&str> = find(&observations, "formatter")
            .iter()
            .map(|o| o.value.as_str())
            .collect();
        assert!(formatters.contains(&"prettier"));
        let builds: Vec<&str> = find(&observations, "build tool")
            .iter()
            .map(|o| o.value.as_str())
            .collect();
        assert!(builds.contains(&"make"));
        assert!(builds.contains(&"tsc"));
    }

    #[test]
    fn chained_commands_split_into_segments() {
        let observations = extract(vec![term_step(
            "cargo fmt --all && cargo clippy && cargo test",
        )]);
        assert_eq!(count_key(&observations, "formatter"), 1);
        assert_eq!(count_key(&observations, "linter"), 1);
        assert_eq!(count_key(&observations, "test runner"), 1);
    }

    #[test]
    fn file_write_emits_language_naming_indentation_and_rewrite_style() {
        let content =
            "fn main() {\n    let x = 1;\n    if x > 0 {\n        println!(\"{x}\");\n    }\n}\n";
        let observations = extract(vec![write_step("src/taste_extraction.rs", content)]);
        let language = find(&observations, "primary language");
        assert_eq!(language.len(), 1);
        assert_eq!(language[0].value, "Rust");
        let naming = find(&observations, "file naming");
        assert_eq!(naming.len(), 1);
        assert_eq!(naming[0].value, "snake_case");
        let indent = find(&observations, "indentation");
        assert_eq!(indent.len(), 1);
        assert_eq!(indent[0].value, "4 spaces");
        let style = find(&observations, "edit style");
        assert_eq!(style.len(), 1);
        assert_eq!(style[0].value, "full file rewrites");
    }

    #[test]
    fn file_write_append_skips_rewrite_style() {
        let observations = extract(vec![step(
            "file_write",
            json!({ "path": "notes.txt", "content": "line", "append": true }),
        )]);
        assert_eq!(count_key(&observations, "edit style"), 0);
    }

    #[test]
    fn patch_emits_targeted_patches_and_language() {
        let observations = extract(vec![patch_step(
            "src/main.rs",
            "let x = 1;",
            "    let x = 2;\n    let y = 3;",
        )]);
        let style = find(&observations, "edit style");
        assert_eq!(style.len(), 1);
        assert_eq!(style[0].value, "targeted patches");
        assert_eq!(find(&observations, "primary language")[0].value, "Rust");
        assert_eq!(find(&observations, "indentation")[0].value, "4 spaces");
    }

    #[test]
    fn edit_block_emits_targeted_patches_and_indentation() {
        let observations = extract(vec![edit_block_step(
            "components/nav-bar.tsx",
            json!([{ "search": "old", "replace": "  new" }]),
        )]);
        let style = find(&observations, "edit style");
        assert_eq!(style.len(), 1);
        assert_eq!(style[0].value, "targeted patches");
        assert_eq!(
            find(&observations, "primary language")[0].value,
            "TypeScript"
        );
        assert_eq!(find(&observations, "file naming")[0].value, "kebab-case");
        assert_eq!(find(&observations, "indentation")[0].value, "2 spaces");
    }

    #[test]
    fn indentation_detects_tabs_two_spaces_and_ambiguity() {
        assert_eq!(
            detect_indentation("fn f() {\n\tlet x = 1;\n}"),
            Some("tabs")
        );
        assert_eq!(
            detect_indentation("function f() {\n  let x = 1;\n  if (x) {\n    go();\n  }\n}"),
            Some("2 spaces")
        );
        // Shallowest indent of 8 could be 2x4 or 4x2 — no signal.
        assert_eq!(detect_indentation("a\n        b\n"), None);
        assert_eq!(detect_indentation("flat\n"), None);
    }

    #[test]
    fn file_naming_conventions() {
        assert_eq!(classify_identifier("taste_extraction"), Some("snake_case"));
        assert_eq!(classify_identifier("nav-bar"), Some("kebab-case"));
        assert_eq!(classify_identifier("formatDate"), Some("camelCase"));
        assert_eq!(classify_identifier("MainPanel"), Some("PascalCase"));
        assert_eq!(classify_identifier("mod"), None);
        assert_eq!(classify_identifier("FOO_BAR"), None);
        assert_eq!(classify_identifier("x"), None);
    }

    #[test]
    fn language_mapping_skips_config_formats() {
        assert_eq!(language_from_path("src/lib.rs"), Some("Rust"));
        assert_eq!(language_from_path("app/component.tsx"), Some("TypeScript"));
        assert_eq!(language_from_path("kerux.example.toml"), None);
        assert_eq!(language_from_path("README.md"), None);
        assert_eq!(language_from_path("noext"), None);
    }

    #[test]
    fn git_commit_styles_classified() {
        let staged = extract(vec![term_step(
            "git add crates/kerux-core/src/lib.rs && git commit -m 'feat: x'",
        )]);
        let style = find(&staged, "commit style");
        assert_eq!(style.len(), 1);
        assert_eq!(style[0].value, "staged per-file commits");

        let bulk = extract(vec![term_step("git add -A && git commit -m 'wip'")]);
        assert_eq!(find(&bulk, "commit style")[0].value, "bulk commits");

        let bulk_commit_a = extract(vec![term_step("git commit -am 'wip'")]);
        assert_eq!(
            find(&bulk_commit_a, "commit style")[0].value,
            "bulk commits"
        );

        let plain_commit = extract(vec![term_step("git commit -m 'chore: docs'")]);
        assert_eq!(count_key(&plain_commit, "commit style"), 0);
    }

    #[test]
    fn test_discipline_requires_prior_edit() {
        let after_edit = extract(vec![
            patch_step("src/lib.rs", "a", "b"),
            term_step("cargo test"),
        ]);
        let discipline = find(&after_edit, "test discipline");
        assert_eq!(discipline.len(), 1);
        assert_eq!(discipline[0].value, "runs tests after edits");
        assert_eq!(discipline[0].category, PreferenceCategory::Testing);

        let without_edit = extract(vec![term_step("cargo test")]);
        assert_eq!(count_key(&without_edit, "test discipline"), 0);
    }

    #[test]
    fn test_discipline_fires_once_per_edit_cycle() {
        let observations = extract(vec![
            patch_step("src/lib.rs", "a", "b"),
            term_step("cargo test"),
            term_step("cargo test"),
        ]);
        assert_eq!(count_key(&observations, "test discipline"), 1);
    }

    #[test]
    fn repeat_cap_limits_identical_observations_per_trajectory() {
        let steps: Vec<TrajectoryStep> = (0..5).map(|_| term_step("cargo test")).collect();
        let observations = extract(steps);
        assert_eq!(count_key(&observations, "test runner"), DEFAULT_MAX_REPEATS);

        // Separate trajectories get separate budgets.
        let extractor = TrajectoryPreferenceExtractor::new();
        let trajectories: Vec<Trajectory> = (0..2)
            .map(|_| trajectory(vec![term_step("cargo test")]))
            .collect();
        let observations = extractor.extract(&trajectories);
        assert_eq!(count_key(&observations, "test runner"), 2);
    }

    #[test]
    fn malformed_or_missing_args_yield_nothing_without_panic() {
        let mut broken = TrajectoryStep {
            step: 0,
            thought: None,
            action: Some("terminal".to_string()),
            action_args: Some("not json".to_string()),
            observation: None,
            response: None,
            success: true,
        };
        let observations = extract(vec![broken.clone()]);
        assert!(observations.is_empty());

        broken.action_args = None;
        let observations = extract(vec![broken]);
        assert!(observations.is_empty());
    }

    #[test]
    fn unknown_tools_and_response_steps_are_ignored() {
        let mut response_step = TrajectoryStep {
            step: 1,
            thought: Some("done".to_string()),
            action: None,
            action_args: None,
            observation: None,
            response: Some("All done.".to_string()),
            success: true,
        };
        let observations = extract(vec![
            response_step.clone(),
            step("web_search", json!({ "query": "rust iterators" })),
        ]);
        assert!(observations.is_empty());

        response_step.action = Some("memory_store".to_string());
        let observations = extract(vec![response_step]);
        assert!(observations.is_empty());
    }

    #[test]
    fn empty_trajectory_yields_nothing() {
        let extractor = TrajectoryPreferenceExtractor::new();
        assert!(extractor.extract(&[]).is_empty());
        assert!(extractor.extract(&[trajectory(vec![])]).is_empty());
    }

    #[test]
    fn observed_at_uses_trajectory_timestamp() {
        let observations = extract(vec![term_step("cargo test")]);
        assert_eq!(observations[0].observed_at, 1_700_000_000);
    }

    #[test]
    fn observations_fold_into_profile_with_expected_confidence() {
        let extractor = TrajectoryPreferenceExtractor::new();
        let trajectories: Vec<Trajectory> = (0..3)
            .map(|_| trajectory(vec![term_step("cargo test --workspace")]))
            .collect();
        let observations = extractor.extract(&trajectories);

        let mut profile = TasteProfile::new("kerux");
        profile.apply_observations(&observations);

        let pref = profile.find("test runner").expect("test runner folded");
        assert_eq!(pref.positive, 3);
        assert_eq!(pref.negative, 0);
        assert!((pref.confidence - compute_confidence(3, 0)).abs() < f32::EPSILON);
        assert_eq!(pref.source, PreferenceSource::Extracted);
        assert_eq!(pref.first_observed_at, 1_700_000_000);
    }

    #[test]
    fn competing_edit_styles_coexist_until_render() {
        let observations = extract(vec![
            patch_step("src/a.rs", "x", "y"),
            patch_step("src/b.rs", "x", "y"),
            write_step("src/c.rs", "fn main() {}\n"),
        ]);
        let styles = find(&observations, "edit style");
        assert_eq!(styles.len(), 3);
        assert_eq!(
            styles
                .iter()
                .filter(|o| o.value == "targeted patches")
                .count(),
            2
        );
        assert_eq!(
            styles
                .iter()
                .filter(|o| o.value == "full file rewrites")
                .count(),
            1
        );

        let mut profile = TasteProfile::new("kerux");
        profile.apply_observations(&observations);
        // Both edit-style values coexist alongside the language signal;
        // render picks the stronger value per key.
        assert_eq!(profile.preferences.len(), 3);
        let block = profile.render_prompt_block(0.0, 10).unwrap();
        assert!(block.contains("targeted patches"));
        assert!(!block.contains("full file rewrites"));
    }
}
