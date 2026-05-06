//! Patch tool
//!
//! A file patch tool that does targeted find-and-replace.
//! Matches Python's patch tool from file_tools.py.
//! Supports exact matching with a fuzzy fallback that normalizes
//! whitespace and line endings.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::schema::ToolSchema;
use crate::tools::file_tools::ensure_safe_path;
use crate::tools::{HermesTool, ToolContext, ToolResult};

/// Arguments for the patch tool
#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchArgs {
    /// Path to the file to patch
    path: String,
    /// Exact string to find in the file
    find: String,
    /// Replacement string
    replace: String,
}

/// Tool for targeted find-and-replace patching of files
pub struct PatchTool;

/// Normalize a string for fuzzy matching: trim each line, collapse
/// runs of whitespace, and normalize line endings to \n.
fn normalize_whitespace(s: &str) -> String {
    s.replace("\r\n", "\n")
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Attempt a fuzzy replacement. Normalizes both the file content and
/// the `find` string, locates the match in the normalized content,
/// then maps back to the original content using a line-based approach.
fn fuzzy_replace(content: &str, find: &str, replace: &str) -> Option<(String, usize)> {
    let normalized_content = normalize_whitespace(content);
    let normalized_find = normalize_whitespace(find);

    if normalized_find.is_empty() {
        return None;
    }

    // Count occurrences in normalized form
    let count = normalized_content.matches(&normalized_find).count();
    if count == 0 {
        return None;
    }

    // Work line-by-line: find which normalized lines match the normalized find lines
    let content_lines: Vec<&str> = content.lines().collect();
    let find_lines: Vec<&str> = normalized_find.lines().collect();

    if find_lines.is_empty() {
        return None;
    }

    let mut replacements = 0;
    let mut result_lines: Vec<String> = Vec::new();
    let mut i = 0;

    while i < content_lines.len() {
        // Check if lines starting at `i` match the normalized find lines
        if i + find_lines.len() <= content_lines.len() {
            let mut matched = true;
            for (j, find_line) in find_lines.iter().enumerate() {
                let content_normalized = content_lines[i + j]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if content_normalized != *find_line {
                    matched = false;
                    break;
                }
            }

            if matched {
                // Replace this block with the replacement text
                for (idx, line) in replace.lines().enumerate() {
                    if idx > 0 || !replace.is_empty() {
                        result_lines.push(line.to_string());
                    }
                }
                // Handle the case where replace is empty (deletion)
                if replace.is_empty() {
                    // Don't add anything
                    if !result_lines.is_empty() || i > 0 {
                        // already handled
                    }
                }
                i += find_lines.len();
                replacements += 1;
                continue;
            }
        }

        result_lines.push(content_lines[i].to_string());
        i += 1;
    }

    if replacements > 0 {
        // Preserve the original trailing newline if present
        let mut result = result_lines.join("\n");
        if content.ends_with('\n') && !result.ends_with('\n') {
            result.push('\n');
        }
        Some((result, replacements))
    } else {
        None
    }
}

#[async_trait]
impl HermesTool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a targeted find-and-replace patch to a file. Finds an exact string \
        in the file and replaces it. Falls back to fuzzy matching (normalized whitespace \
        and line endings) if an exact match is not found."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<PatchArgs>("patch", "Find and replace in a file")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: PatchArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("patch", format!("Invalid arguments: {}", e)),
        };

        let path = match ensure_safe_path(&args.path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error("patch", e),
        };

        if !path.exists() {
            return ToolResult::error("patch", format!("File not found: {}", args.path));
        }

        if !path.is_file() {
            return ToolResult::error("patch", format!("Path is not a file: {}", args.path));
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolResult::error("patch", format!("Failed to read file: {}", e)),
        };

        // Try exact match first
        let exact_count = content.matches(&args.find).count();

        let (new_content, replacements, match_type) = if exact_count > 0 {
            let replaced = content.replace(&args.find, &args.replace);
            (replaced, exact_count, "exact")
        } else {
            // Fuzzy fallback
            match fuzzy_replace(&content, &args.find, &args.replace) {
                Some((replaced, count)) => (replaced, count, "fuzzy"),
                None => {
                    return ToolResult::error(
                        "patch",
                        format!(
                            "Find string not found in file '{}' (tried both exact and fuzzy matching)",
                            args.path
                        ),
                    );
                }
            }
        };

        // Write the patched content back
        match std::fs::write(&path, &new_content) {
            Ok(_) => {}
            Err(e) => {
                return ToolResult::error("patch", format!("Failed to write file: {}", e));
            }
        }

        ToolResult::success(
            "patch",
            serde_json::json!({
                "path": args.path,
                "replacements": replacements,
                "matchType": match_type,
                "fileSize": new_content.len(),
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    fn default_context() -> ToolContext {
        ToolContext::default()
    }

    fn create_temp_file(content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "hermes_patch_test_{}.txt",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn test_patch_exact_match() {
        let path = create_temp_file("Hello, world!\nGoodbye, world!\n");
        let path_str = path.to_str().unwrap().to_string();

        let tool = PatchTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path_str,
                    "find": "Hello, world!",
                    "replace": "Hi, world!"
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["replacements"], 1);
        assert_eq!(parsed["matchType"], "exact");

        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("Hi, world!"));
        assert!(!updated.contains("Hello, world!"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_patch_file_not_found() {
        let tool = PatchTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": "/nonexistent/file.txt",
                    "find": "a",
                    "replace": "b"
                }),
                default_context(),
            )
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("File not found"));
    }

    #[tokio::test]
    async fn test_patch_find_not_found() {
        let path = create_temp_file("Some content here\n");
        let path_str = path.to_str().unwrap().to_string();

        let tool = PatchTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path_str,
                    "find": "nonexistent string",
                    "replace": "replacement"
                }),
                default_context(),
            )
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not found"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_patch_multiple_replacements() {
        let path = create_temp_file("foo bar foo baz foo\n");
        let path_str = path.to_str().unwrap().to_string();

        let tool = PatchTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path_str,
                    "find": "foo",
                    "replace": "qux"
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["replacements"], 3);

        let updated = std::fs::read_to_string(&path).unwrap();
        assert_eq!(updated, "qux bar qux baz qux\n");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_patch_fuzzy_whitespace() {
        let path = create_temp_file("  fn  main()  {\n    println!(\"hi\");\n  }\n");
        let path_str = path.to_str().unwrap().to_string();

        let tool = PatchTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path_str,
                    "find": "fn main() {\nprintln!(\"hi\");\n}",
                    "replace": "fn main() {\n    println!(\"hello\");\n}"
                }),
                default_context(),
            )
            .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(parsed["matchType"], "fuzzy");

        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("hello"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_normalize_whitespace() {
        assert_eq!(
            normalize_whitespace("  hello   world  \r\n  foo  bar  "),
            "hello world\nfoo bar"
        );
    }

    #[test]
    fn test_fuzzy_replace_basic() {
        let content = "  fn  foo()  {\n    bar();\n  }\n";
        let find = "fn foo() {\nbar();\n}";
        let replace = "fn foo() {\n    baz();\n}";

        let result = fuzzy_replace(content, find, replace);
        assert!(result.is_some());
        let (replaced, count) = result.unwrap();
        assert_eq!(count, 1);
        assert!(replaced.contains("baz"));
    }

    #[tokio::test]
    async fn test_patch_traversal() {
        let tool = PatchTool;
        let ctx = ToolContext::default();

        let args = serde_json::json!({
            "path": "/etc/passwd",
            "find": "root",
            "replace": "evil"
        });

        let result = tool.execute(args, ctx).await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Access denied"));
    }
}
