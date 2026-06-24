use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{PermissionLevel, ToolDef, ToolResult};

/// List the contents of a directory.
pub struct ListDirectory;

#[async_trait]
impl ToolDef for ListDirectory {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the contents of a directory. Returns file and subdirectory names. \
         Subdirectories are suffixed with '/'. Optionally recurse into subdirectories."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory path to list (absolute or relative to the working directory)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to list recursively (default: false). Limited to 1000 entries."
                }
            },
            "required": ["path"]
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    async fn execute(&self, args: &Value, _cancel: CancellationToken) -> ToolResult {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::Err("Missing required parameter: path".into()),
        };

        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if recursive {
            self.list_recursive(path).await
        } else {
            self.list_flat(path).await
        }
    }

    fn display_summary(&self, args: &Value) -> String {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if recursive {
            format!("list_directory: {} (recursive)", path)
        } else {
            format!("list_directory: {}", path)
        }
    }
}

impl ListDirectory {
    async fn list_flat(&self, path: &str) -> ToolResult {
        let mut entries = match tokio::fs::read_dir(path).await {
            Ok(e) => e,
            Err(e) => {
                return ToolResult::Err(format!("Failed to read directory '{}': {}", path, e));
            }
        };

        let mut names = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry
                .file_type()
                .await
                .map(|ft| ft.is_dir())
                .unwrap_or(false);
            if is_dir {
                names.push(format!("{}/", name));
            } else {
                names.push(name);
            }
        }

        names.sort();
        ToolResult::ok_text(names.join("\n"))
    }

    async fn list_recursive(&self, base: &str) -> ToolResult {
        const MAX_ENTRIES: usize = 1000;
        let mut entries = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(base)];

        while let Some(dir) = stack.pop() {
            if entries.len() >= MAX_ENTRIES {
                entries.push(format!("[truncated at {} entries]", MAX_ENTRIES));
                break;
            }

            let mut read_dir = match tokio::fs::read_dir(&dir).await {
                Ok(d) => d,
                Err(_) => continue,
            };

            let mut children = Vec::new();
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip hidden files/dirs
                if name.starts_with('.') {
                    continue;
                }
                let path = entry.path();
                let is_dir = entry
                    .file_type()
                    .await
                    .map(|ft| ft.is_dir())
                    .unwrap_or(false);

                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                if is_dir {
                    entries.push(format!("{}/", rel));
                    children.push(path);
                } else {
                    entries.push(rel);
                }

                if entries.len() >= MAX_ENTRIES {
                    break;
                }
            }

            // Sort children for deterministic output
            children.sort();
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }

        entries.sort();
        ToolResult::ok_text(entries.join("\n"))
    }
}
