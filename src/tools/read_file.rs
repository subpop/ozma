use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{PermissionLevel, ToolDef, ToolResult};

/// Read the contents of a file from disk.
pub struct ReadFile;

#[async_trait]
impl ToolDef for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file at the given path. Returns the file content as text. \
         For large files, use the offset and limit parameters to read a specific range of lines."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to read (absolute or relative to the working directory)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed, default: 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default: 2000)"
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

        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::Err(format!("Failed to read file '{}': {}", path, e)),
        };

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        // Apply offset and limit (1-indexed)
        let start = (offset - 1).min(total_lines);
        let end = (start + limit).min(total_lines);
        let selected: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", start + i + 1, line))
            .collect();

        let mut result = selected.join("\n");
        if end < total_lines {
            result.push_str(&format!(
                "\n\n[Showing lines {}-{} of {}. Use offset/limit to read more.]",
                start + 1,
                end,
                total_lines
            ));
        }

        ToolResult::ok_text(result)
    }

    fn display_summary(&self, args: &Value) -> String {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        format!("read_file: {}", path)
    }
}
