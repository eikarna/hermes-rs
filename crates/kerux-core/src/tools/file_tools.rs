//! File operation tools
//!
//! Tools for reading, writing, searching, and listing files.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

use crate::schema::ToolSchema;
use crate::tools::{KeruxTool, ToolContext, ToolResult};

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
impl KeruxTool for FileReadTool {
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

        let path = PathBuf::from(&args.path);

        if !path.exists() {
            return ToolResult::error("file_read", format!("File not found: {}", args.path));
        }

        if !path.is_file() {
            return ToolResult::error("file_read", format!("Path is not a file: {}", args.path));
        }

        match tokio::fs::read_to_string(&path).await {
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
impl KeruxTool for FileWriteTool {
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

        let path = PathBuf::from(&args.path);

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            let parent_exists = tokio::fs::try_exists(parent).await.unwrap_or(false);
            if !parent_exists {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolResult::error(
                        "file_write",
                        format!("Failed to create directory: {}", e),
                    );
                }
            }
        }

        let result = if args.append.unwrap_or(false) {
            let file_result = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await;

            match file_result {
                Ok(mut f) => {
                    use tokio::io::AsyncWriteExt;
                    f.write_all(args.content.as_bytes()).await
                }
                Err(e) => Err(e),
            }
        } else {
            tokio::fs::write(&path, &args.content).await
        };

        match result {
            Ok(_) => {
                let metadata = tokio::fs::metadata(&path).await.ok();
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
impl KeruxTool for FileSearchTool {
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

        let path = PathBuf::from(&args.path);
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

        let max_results = args.max_results.unwrap_or(100);

        if !path.exists() {
            return ToolResult::error("file_search", format!("Path does not exist: {}", args.path));
        }

        let results = tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let mut stack = vec![path];

            while let Some(current_path) = stack.pop() {
                if results.len() >= max_results {
                    break;
                }

                if current_path.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&current_path) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_dir() {
                                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                    if !name.starts_with('.')
                                        && name != "node_modules"
                                        && name != "target"
                                        && name != "__pycache__"
                                    {
                                        stack.push(path);
                                    }
                                }
                            } else if path.is_file() {
                                stack.push(path);
                            }
                        }
                    }
                } else if current_path.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&current_path) {
                        for (line_num, line) in content.lines().enumerate() {
                            if re.is_match(line) {
                                results.push(serde_json::json!({
                                    "file": current_path.to_string_lossy(),
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
            results
        })
        .await
        .unwrap_or_default();

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
impl KeruxTool for FileListTool {
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

        let path = PathBuf::from(&args.path);

        if !path.exists() {
            return ToolResult::error("file_list", format!("Path does not exist: {}", args.path));
        }

        if !path.is_dir() {
            return ToolResult::error(
                "file_list",
                format!("Path is not a directory: {}", args.path),
            );
        }

        let (recursive, include_hidden) = (
            args.recursive.unwrap_or(false),
            args.include_hidden.unwrap_or(false),
        );

        let mut entries = tokio::task::spawn_blocking(move || {
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

            list_recursive(&path, &mut entries, recursive, include_hidden);
            entries
        })
        .await
        .unwrap_or_default();

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
