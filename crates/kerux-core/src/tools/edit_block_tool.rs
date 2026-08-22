//! Edit block tool
//!
//! Applies Aider-style SEARCH/REPLACE edits: multiple ordered find-and-replace
//! operations on a single file, written back atomically. Each edit tries an
//! exact match first and falls back to fuzzy whitespace-normalized matching
//! (shared with the `patch` tool).
//!
//! Also provides `parse_edit_blocks`, a pure parser for the classic Aider text
//! format so callers can extract structured edits from raw model output:
//!
//! ```text
//! path/to/file.rs
//! <<<<<<< SEARCH
//! old code
//! =======
//! new code
//! >>>>>>> REPLACE
//! ```

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

use crate::schema::ToolSchema;
use crate::tools::patch_tool::fuzzy_replace;
use crate::tools::{KeruxTool, ToolContext, ToolResult};

const SEARCH_MARKER: &str = "<<<<<<< SEARCH";
const DIVIDER_MARKER: &str = "=======";
const REPLACE_MARKER: &str = ">>>>>>> REPLACE";

/// A single structured edit extracted from an edit block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditBlock {
    pub path: String,
    pub search: String,
    pub replace: String,
}

/// Parse Aider-style edit blocks from raw model output.
///
/// A block is a file path line followed by `<<<<<<< SEARCH`, the search text,
/// `=======`, the replacement text, and `>>>>>>> REPLACE`. Tolerant: text
/// outside blocks is ignored, and unterminated trailing blocks are dropped.
pub fn parse_edit_blocks(text: &str) -> Vec<EditBlock> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    let mut current_path: Option<String> = None;

    while i < lines.len() {
        let line = lines[i].trim_end();
        if line == SEARCH_MARKER {
            // search text until divider
            let mut search = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim_end() != DIVIDER_MARKER {
                // nested SEARCH marker means the block is malformed; abort it
                if lines[i].trim_end() == SEARCH_MARKER {
                    break;
                }
                search.push(lines[i]);
                i += 1;
            }
            if i >= lines.len() || lines[i].trim_end() != DIVIDER_MARKER {
                continue;
            }
            // replace text until end marker
            let mut replace = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim_end() != REPLACE_MARKER {
                replace.push(lines[i]);
                i += 1;
            }
            if i >= lines.len() {
                break; // unterminated block
            }
            i += 1; // consume REPLACE_MARKER
            if let Some(path) = &current_path {
                blocks.push(EditBlock {
                    path: path.clone(),
                    search: search.join("\n"),
                    replace: replace.join("\n"),
                });
            }
            continue; // next iteration starts at the line after the block
        } else if !line.is_empty()
            && !line.starts_with("```")
            && !line.starts_with('<')
            && !line.contains(' ')
        {
            // candidate file path line
            current_path = Some(line.to_string());
        }
        i += 1;
    }
    blocks
}

/// Apply ordered edits to `content`. Returns the new content and per-edit
/// match types ("exact" or "fuzzy"). Fails on the first edit that matches
/// nothing, leaving the file untouched.
fn apply_edits(
    content: &str,
    edits: &[(String, String)],
) -> Result<(String, Vec<&'static str>), String> {
    let mut content = content.to_string();
    let mut match_types = Vec::with_capacity(edits.len());
    for (search, replace) in edits {
        if content.contains(search.as_str()) {
            content = content.replacen(search.as_str(), replace, 1);
            match_types.push("exact");
        } else if let Some((replaced, _)) = fuzzy_replace(&content, search, replace) {
            content = replaced;
            match_types.push("fuzzy");
        } else {
            return Err(format!(
                "Search text not found (tried exact and fuzzy): {:.120}",
                search
            ));
        }
    }
    Ok((content, match_types))
}

/// Arguments for the edit_block tool.
#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditBlockArgs {
    /// Path to the file to edit
    path: String,
    /// Ordered edits to apply; each is a search/replace pair
    edits: Vec<EditPair>,
}

/// One search/replace pair.
#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditPair {
    /// Exact text to find in the file
    search: String,
    /// Replacement text (empty string deletes the matched block)
    replace: String,
}

/// Tool that applies multiple ordered edits to a file atomically.
pub struct EditBlockTool;

#[async_trait]
impl KeruxTool for EditBlockTool {
    fn name(&self) -> &str {
        "edit_block"
    }

    fn description(&self) -> &str {
        "Apply multiple ordered search/replace edits to a single file. Each edit finds an \
        exact string first, then falls back to fuzzy whitespace-normalized matching. All \
        edits are applied together; if any edit fails to match, the file is left unchanged."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<EditBlockArgs>(
            "edit_block",
            "Apply ordered search/replace edits to a file",
        )
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: EditBlockArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("edit_block", format!("Invalid arguments: {}", e)),
        };

        if args.edits.is_empty() {
            return ToolResult::error("edit_block", "No edits provided");
        }

        let path = PathBuf::from(&args.path);
        if !path.is_file() {
            return ToolResult::error("edit_block", format!("File not found: {}", args.path));
        }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error("edit_block", format!("Failed to read file: {}", e))
            }
        };

        let edits: Vec<(String, String)> = args
            .edits
            .iter()
            .map(|e| (e.search.clone(), e.replace.clone()))
            .collect();

        let (new_content, match_types) = match apply_edits(&content, &edits) {
            Ok(ok) => ok,
            Err(e) => return ToolResult::error("edit_block", format!("{}: {}", args.path, e)),
        };

        if let Err(e) = tokio::fs::write(&path, &new_content).await {
            return ToolResult::error("edit_block", format!("Failed to write file: {}", e));
        }

        ToolResult::success(
            "edit_block",
            serde_json::json!({
                "path": args.path,
                "edits": edits.len(),
                "matchTypes": match_types,
                "fileSize": new_content.len(),
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(timestamp: u128) -> PathBuf {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "kerux_editblock_test_{}_{}_{}.txt",
            std::process::id(),
            timestamp,
            sequence
        ))
    }

    #[test]
    fn parses_single_block() {
        let text = "Here is the change:\nsrc/main.rs\n<<<<<<< SEARCH\nfn old() {}\n=======\nfn new() {}\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].path, "src/main.rs");
        assert_eq!(blocks[0].search, "fn old() {}");
        assert_eq!(blocks[0].replace, "fn new() {}");
    }

    #[test]
    fn parses_multiple_blocks_and_files() {
        let text = "a.rs\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE\nb.rs\n<<<<<<< SEARCH\n1\n=======\n2\n>>>>>>> REPLACE\n";
        let blocks = parse_edit_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].path, "a.rs");
        assert_eq!(blocks[1].path, "b.rs");
    }

    #[test]
    fn drops_unterminated_block() {
        let text = "a.rs\n<<<<<<< SEARCH\nx\n=======\ny\n";
        assert!(parse_edit_blocks(text).is_empty());
    }

    #[test]
    fn ignores_block_without_path() {
        let text = "Some prose.\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE\n";
        assert!(parse_edit_blocks(text).is_empty());
    }

    #[test]
    fn apply_edits_exact_then_fuzzy() {
        let content = "fn a() {}\n  fn  b()  {}\n";
        let edits = vec![
            ("fn a() {}".to_string(), "fn alpha() {}".to_string()),
            ("fn b() {}".to_string(), "fn beta() {}".to_string()),
        ];
        let (new_content, kinds) = apply_edits(content, &edits).unwrap();
        assert!(new_content.contains("fn alpha() {}"));
        assert!(new_content.contains("fn beta() {}"));
        assert_eq!(kinds, vec!["exact", "fuzzy"]);
    }

    #[test]
    fn apply_edits_fails_atomically() {
        let content = "fn a() {}\n";
        let edits = vec![
            ("fn a() {}".to_string(), "fn alpha() {}".to_string()),
            ("missing".to_string(), "x".to_string()),
        ];
        assert!(apply_edits(content, &edits).is_err());
    }

    fn create_temp_file(content: &str) -> PathBuf {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = temp_path(timestamp);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn temp_paths_are_unique_for_identical_timestamps() {
        let first = temp_path(42);
        let second = temp_path(42);
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn tool_applies_multiple_edits() {
        let path = create_temp_file("one\ntwo\nthree\n");
        let tool = EditBlockTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path.to_str().unwrap(),
                    "edits": [
                        {"search": "one", "replace": "1"},
                        {"search": "three", "replace": "3"},
                    ]
                }),
                ToolContext::default(),
            )
            .await;
        assert!(result.success);
        let updated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(updated, "1\ntwo\n3\n");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn tool_leaves_file_untouched_on_failure() {
        let path = create_temp_file("one\ntwo\n");
        let tool = EditBlockTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path.to_str().unwrap(),
                    "edits": [
                        {"search": "one", "replace": "1"},
                        {"search": "nope", "replace": "x"},
                    ]
                }),
                ToolContext::default(),
            )
            .await;
        assert!(!result.success);
        let unchanged = std::fs::read_to_string(&path).unwrap();
        assert_eq!(unchanged, "one\ntwo\n");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn tool_rejects_empty_edits() {
        let path = create_temp_file("x\n");
        let tool = EditBlockTool;
        let result = tool
            .execute(
                serde_json::json!({"path": path.to_str().unwrap(), "edits": []}),
                ToolContext::default(),
            )
            .await;
        assert!(!result.success);
        let _ = std::fs::remove_file(&path);
    }
}
