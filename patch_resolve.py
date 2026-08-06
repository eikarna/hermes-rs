import sys

def patch_file(file_path, search_content, replace_content):
    with open(file_path, 'r') as f:
        content = f.read()

    if search_content in content:
        new_content = content.replace(search_content, replace_content)
        with open(file_path, 'w') as f:
            f.write(new_content)
        print("Patch applied successfully.")
    else:
        print("Search content not found.")

search1 = """        let max_results = args.max_results.unwrap_or(100);

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
        .unwrap_or_default();"""

replace1 = """        let max_results = args.max_results.unwrap_or(100);

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
        .unwrap_or_default();"""

search2 = """        let recursive = args.recursive.unwrap_or(false);
        let include_hidden = args.include_hidden.unwrap_or(false);

        let entries = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            let mut stack = vec![path];

            while let Some(current_dir) = stack.pop() {
                let read_dir = match std::fs::read_dir(&current_dir) {
                    Ok(rd) => rd,
                    Err(_) => continue,
                };

                for entry in read_dir.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();

                    // Skip hidden files/dirs unless requested
                    if !include_hidden && name.starts_with('.') {
                        continue;
                    }

                    let path = entry.path();
                    let metadata = entry.metadata().ok();
                    let is_dir = path.is_dir();

                    let entry_json = serde_json::json!({
                        "name": name,
                        "path": path.to_string_lossy(),
                        "is_dir": is_dir,
                        "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                        "modified": metadata.as_ref()
                            .and_then(|m| m.modified().ok())
                            .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                    });

                    entries.push(entry_json);

                    if recursive && is_dir {
                        stack.push(path);
                    }
                }
            }
            entries
        })
        .await
        .unwrap_or_default();"""

replace2 = """        let recursive = args.recursive.unwrap_or(false);
        let include_hidden = args.include_hidden.unwrap_or(false);

        let entries = tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            let mut stack = vec![path];

            while let Some(current_dir) = stack.pop() {
                let read_dir = match std::fs::read_dir(&current_dir) {
                    Ok(rd) => rd,
                    Err(_) => continue,
                };

                for entry in read_dir.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();

                    // Skip hidden files/dirs unless requested
                    if !include_hidden && name.starts_with('.') {
                        continue;
                    }

                    let path = entry.path();
                    let metadata = entry.metadata().ok();
                    let is_dir = path.is_dir();

                    let entry_json = serde_json::json!({
                        "name": name,
                        "path": path.to_string_lossy(),
                        "is_dir": is_dir,
                        "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                        "modified": metadata.as_ref()
                            .and_then(|m| m.modified().ok())
                            .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                    });

                    entries.push(entry_json);

                    if recursive && is_dir {
                        stack.push(path);
                    }
                }
            }
            entries
        })
        .await
        .unwrap_or_default();"""


patch_file("crates/hermes-core/src/tools/file_tools.rs", search1, replace1)
patch_file("crates/hermes-core/src/tools/file_tools.rs", search2, replace2)
