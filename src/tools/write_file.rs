use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;
use tokio_util::sync::CancellationToken;

use super::{PermissionLevel, ToolDef, ToolResult};

/// Write content to a file on disk.
pub struct WriteFile;

#[async_trait]
impl ToolDef for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file at the given path. Creates the file if it doesn't exist, \
         or overwrites it if it does. Creates parent directories as needed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to write to (absolute or relative to the working directory)"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Destructive
    }

    async fn execute(&self, args: &Value, _cancel: CancellationToken) -> ToolResult {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::Err("Missing required parameter: path".into()),
        };

        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::Err("Missing required parameter: content".into()),
        };

        // Create parent directories if needed
        if let Some(parent) = Path::new(path).parent() {
            if !parent.exists() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolResult::Err(format!(
                        "Failed to create directories for '{}': {}",
                        path, e
                    ));
                }
            }
        }

        match tokio::fs::write(path, content).await {
            Ok(()) => {
                let lines = content.lines().count();
                let bytes = content.len();
                ToolResult::ok_text(format!(
                    "Successfully wrote {} bytes ({} lines) to '{}'",
                    bytes, lines, path
                ))
            }
            Err(e) => ToolResult::Err(format!("Failed to write file '{}': {}", path, e)),
        }
    }

    fn display_summary(&self, args: &Value) -> String {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let size = args
            .get("content")
            .and_then(|v| v.as_str())
            .map(|c| format!("{} bytes", c.len()))
            .unwrap_or_else(|| "?".to_string());
        format!("write_file: {} ({})", path, size)
    }
}
