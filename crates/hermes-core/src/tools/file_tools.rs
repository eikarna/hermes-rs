//! File operation tools
//!
//! Tools for reading, writing, searching, and listing files.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

use crate::schema::ToolSchema;

/// Ensure a path is safe and within the current workspace (CWD)
pub(crate) fn ensure_safe_path(path_str: &str) -> Result<PathBuf, String> {
    let base = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("Failed to get workspace root: {}", e))?;

    let path = PathBuf::from(path_str);
    let joined = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };

    // If it exists, canonicalize it to resolve symlinks and ..
    if joined.exists() {
        let canonical = joined
            .canonicalize()
            .map_err(|e| format!("Invalid path: {}", e))?;
        if !canonical.starts_with(&base) {
            return Err(format!(
                "Access denied: path {} resolves outside workspace",
                path_str
            ));
        }
        Ok(canonical)
    } else {
        // For new files or paths that don't exist yet, we normalize the path lexically
        let mut normalized = PathBuf::new();
        for component in joined.components() {
            match component {
                std::path::Component::Prefix(p) => normalized.push(p.as_os_str()),
                std::path::Component::RootDir => {
                    normalized.push(std::path::MAIN_SEPARATOR.to_string())
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(format!(
                            "Access denied: path {} attempts to escape root",
                            path_str
                        ));
                    }
                }
                std::path::Component::Normal(c) => normalized.push(c),
            }
        }

        if !normalized.starts_with(&base) {
            return Err(format!(
                "Access denied: path {} is outside workspace",
                path_str
            ));
        }
        Ok(normalized)
    }
}
use crate::tools::{HermesTool, ToolContext, ToolResult};

/// Tool for reading file contents
pub struct FileReadTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[async_trait]
impl HermesTool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports partial reads with offset and limit parameters."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<FileReadArgs>("file_read", "Read file contents")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: FileReadArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("file_read", format!("Invalid arguments: {}", e)),
        };

        let path = match ensure_safe_path(&args.path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error("file_read", e),
        };

        if !path.exists() {
            return ToolResult::error("file_read", format!("File not found: {}", args.path));
        }

        if !path.is_file() {
            return ToolResult::error("file_read", format!("Path is not a file: {}", args.path));
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let offset = args.offset.unwrap_or(0);
                let limit = args.limit.unwrap_or(usize::MAX);

                let lines: Vec<&str> = content.lines().skip(offset).take(limit).collect();
                let result = lines.join("\n");

                ToolResult::success(
                    "file_read",
                    serde_json::json!({
                        "path": args.path,
                        "content": result,
                        "length": result.len(),
                        "total_lines": content.lines().count()
                    }),
                )
            }
            Err(e) => ToolResult::error("file_read", format!("Failed to read file: {}", e)),
        }
    }
}

/// Tool for writing content to a file
pub struct FileWriteTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileWriteArgs {
    path: String,
    content: String,
    append: Option<bool>,
}

#[async_trait]
impl HermesTool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Supports creating new files or overwriting existing ones. Use append=true to add to existing files."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<FileWriteArgs>("file_write", "Write content to a file")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: FileWriteArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("file_write", format!("Invalid arguments: {}", e)),
        };

        let path = match ensure_safe_path(&args.path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error("file_write", e),
        };

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::error(
                        "file_write",
                        format!("Failed to create directory: {}", e),
                    );
                }
            }
        }

        let result = if args.append.unwrap_or(false) {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(args.content.as_bytes())
                })
        } else {
            std::fs::write(&path, &args.content)
        };

        match result {
            Ok(_) => {
                let metadata = std::fs::metadata(&path).ok();
                ToolResult::success(
                    "file_write",
                    serde_json::json!({
                        "path": args.path,
                        "bytes_written": args.content.len(),
                        "file_size": metadata.map(|m| m.len()).unwrap_or(0)
                    }),
                )
            }
            Err(e) => ToolResult::error("file_write", format!("Failed to write file: {}", e)),
        }
    }
}

/// Tool for searching file contents
pub struct FileSearchTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileSearchArgs {
    path: String,
    pattern: String,
    case_sensitive: Option<bool>,
    max_results: Option<usize>,
}

#[async_trait]
impl HermesTool for FileSearchTool {
    fn name(&self) -> &str {
        "file_search"
    }

    fn description(&self) -> &str {
        "Search for a pattern within files. Recursively searches directories for matching lines."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<FileSearchArgs>("file_search", "Search files for pattern")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: FileSearchArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("file_search", format!("Invalid arguments: {}", e)),
        };

        let path = match ensure_safe_path(&args.path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error("file_search", e),
        };
        let case_sensitive = args.case_sensitive.unwrap_or(true);
        let escaped_pattern = regex::escape(&args.pattern);
        let re = match regex::RegexBuilder::new(&escaped_pattern)
            .case_insensitive(!case_sensitive)
            .build()
        {
            Ok(re) => re,
            Err(e) => {
                return ToolResult::error("file_search", format!("Invalid regex pattern: {}", e))
            }
        };

        let mut results = Vec::new();
        let max_results = args.max_results.unwrap_or(100);

        fn search_recursive(
            dir: &PathBuf,
            re: &regex::Regex,
            results: &mut Vec<serde_json::Value>,
            max_results: usize,
        ) {
            if results.len() >= max_results {
                return;
            }

            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };

            for entry in entries.flatten() {
                if results.len() >= max_results {
                    break;
                }

                let path = entry.path();

                if path.is_dir() {
                    // Skip hidden directories and common non-relevant dirs
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if !name.starts_with('.')
                            && name != "node_modules"
                            && name != "target"
                            && name != "__pycache__"
                        {
                            search_recursive(&path, re, results, max_results);
                        }
                    }
                } else if path.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        for (line_num, line) in content.lines().enumerate() {
                            if re.is_match(line) {
                                results.push(serde_json::json!({
                                    "file": path.to_string_lossy(),
                                    "line": line_num + 1,
                                    "content": line
                                }));

                                if results.len() >= max_results {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        if path.is_dir() {
            search_recursive(&path, &re, &mut results, max_results);
        } else if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for (line_num, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        results.push(serde_json::json!({
                            "file": path.to_string_lossy(),
                            "line": line_num + 1,
                            "content": line
                        }));

                        if results.len() >= max_results {
                            break;
                        }
                    }
                }
            }
        } else {
            return ToolResult::error("file_search", format!("Path does not exist: {}", args.path));
        }

        ToolResult::success(
            "file_search",
            serde_json::json!({
                "pattern": args.pattern,
                "path": args.path,
                "matches": results,
                "count": results.len()
            }),
        )
    }
}

/// Tool for listing directory contents
pub struct FileListTool;

#[derive(JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileListArgs {
    path: String,
    recursive: Option<bool>,
    include_hidden: Option<bool>,
}

#[async_trait]
impl HermesTool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn description(&self) -> &str {
        "List directory contents. Shows files and subdirectories with metadata."
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::from_type::<FileListArgs>("file_list", "List directory contents")
    }

    async fn execute(&self, args: Value, _context: ToolContext) -> ToolResult {
        let args: FileListArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error("file_list", format!("Invalid arguments: {}", e)),
        };

        let path = match ensure_safe_path(&args.path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error("file_list", e),
        };

        if !path.exists() {
            return ToolResult::error("file_list", format!("Path does not exist: {}", args.path));
        }

        if !path.is_dir() {
            return ToolResult::error(
                "file_list",
                format!("Path is not a directory: {}", args.path),
            );
        }

        let mut entries = Vec::new();

        fn list_recursive(
            dir: &PathBuf,
            entries: &mut Vec<serde_json::Value>,
            recursive: bool,
            include_hidden: bool,
        ) {
            let read_dir = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(_) => return,
            };

            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();

                // Skip hidden files/dirs unless requested
                if !include_hidden && name.starts_with('.') {
                    continue;
                }

                let path = entry.path();
                let metadata = entry.metadata().ok();

                let entry_json = serde_json::json!({
                    "name": name,
                    "path": path.to_string_lossy(),
                    "is_dir": path.is_dir(),
                    "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                    "modified": metadata.as_ref()
                        .and_then(|m| m.modified().ok())
                        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                });

                entries.push(entry_json);

                if recursive && path.is_dir() {
                    list_recursive(&path, entries, recursive, include_hidden);
                }
            }
        }

        list_recursive(
            &path,
            &mut entries,
            args.recursive.unwrap_or(false),
            args.include_hidden.unwrap_or(false),
        );

        // Sort: directories first, then by name
        entries.sort_by(|a, b| {
            let a_is_dir = a["is_dir"].as_bool().unwrap_or(false);
            let b_is_dir = b["is_dir"].as_bool().unwrap_or(false);

            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["name"].as_str().unwrap_or("")),
            }
        });

        ToolResult::success(
            "file_list",
            serde_json::json!({
                "path": args.path,
                "entries": entries,
                "count": entries.len()
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    #[tokio::test]
    async fn test_ensure_safe_path() {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();

        // Safe paths
        assert!(ensure_safe_path("Cargo.toml").is_ok());
        assert!(ensure_safe_path("./Cargo.toml").is_ok());

        // Unsafe paths
        let result = ensure_safe_path("/etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Access denied"));

        let result = ensure_safe_path("../../../etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Access denied"));
    }

    #[tokio::test]
    async fn test_file_read_traversal() {
        let tool = FileReadTool;
        let ctx = ToolContext::default();

        let args = serde_json::json!({
            "path": "/etc/passwd"
        });

        let result = tool.execute(args, ctx).await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Access denied"));
    }

    #[tokio::test]
    async fn test_file_write_traversal() {
        let tool = FileWriteTool;
        let ctx = ToolContext::default();

        let args = serde_json::json!({
            "path": "/tmp/evil.sh",
            "content": "echo 'evil'"
        });

        let result = tool.execute(args, ctx).await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Access denied"));
    }
}
