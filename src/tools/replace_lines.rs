use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{PermissionLevel, ToolDef, ToolResult};

/// Replace an inclusive range of lines in an existing text file.
pub struct ReplaceLines;

#[async_trait]
impl ToolDef for ReplaceLines {
    fn name(&self) -> &str {
        "replace_lines"
    }

    fn description(&self) -> &str {
        "Replace a 1-indexed, inclusive range of lines in an existing text file without \
         rewriting the rest. Set content to an empty string to delete the lines. \
         Optionally provide expected_content (the selected lines joined with newlines, \
         without a final newline) to reject edits if the lines have changed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Existing text file path (absolute or relative to the working directory)"
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "First line to replace (1-indexed, inclusive)"
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to replace (1-indexed, inclusive)"
                },
                "content": {
                    "type": "string",
                    "description": "Replacement lines; empty string deletes the selected lines"
                },
                "expected_content": {
                    "type": "string",
                    "description": "Optional original selected lines joined with newlines, without a final newline; abort if they differ"
                }
            },
            "required": ["path", "start_line", "end_line", "content"]
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Destructive
    }

    async fn execute(&self, args: &Value, _cancel: CancellationToken) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::Err("Missing required parameter: path".into());
        };
        let Some(start) = args
            .get("start_line")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
        else {
            return ToolResult::Err("start_line must be a positive integer".into());
        };
        let Some(end) = args
            .get("end_line")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
        else {
            return ToolResult::Err("end_line must be a positive integer".into());
        };
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return ToolResult::Err("Missing required parameter: content".into());
        };
        let expected = match args.get("expected_content") {
            None => None,
            Some(Value::String(text)) => Some(text.as_str()),
            _ => return ToolResult::Err("expected_content must be a string".into()),
        };

        let original = match tokio::fs::read_to_string(path).await {
            Ok(text) => text,
            Err(e) => return ToolResult::Err(format!("Failed to read file '{}': {}", path, e)),
        };
        let replacement = match replace_range(&original, start, end, content, expected) {
            Ok(text) => text,
            Err(e) => return ToolResult::Err(e),
        };

        match tokio::fs::write(path, replacement).await {
            Ok(()) => {
                ToolResult::ok_text(format!("Replaced lines {}-{} in '{}'", start, end, path))
            }
            Err(e) => ToolResult::Err(format!("Failed to write file '{}': {}", path, e)),
        }
    }

    fn display_summary(&self, args: &Value) -> String {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("?");
        let start = args.get("start_line").and_then(Value::as_u64);
        let end = args.get("end_line").and_then(Value::as_u64);
        match (start, end) {
            (Some(start), Some(end)) => format!("replace_lines: {} ({}-{})", path, start, end),
            _ => format!("replace_lines: {}", path),
        }
    }
}

fn replace_range(
    original: &str,
    start: usize,
    end: usize,
    content: &str,
    expected: Option<&str>,
) -> Result<String, String> {
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    if start == 0 || start > end || end > lines.len() {
        return Err(format!(
            "Invalid line range {}-{} (file has {} lines)",
            start,
            end,
            lines.len()
        ));
    }

    if let Some(expected) = expected {
        let selected = lines[start - 1..end]
            .iter()
            .map(|line| line.trim_end_matches(['\r', '\n']))
            .collect::<Vec<_>>()
            .join("\n");
        if selected != expected {
            return Err("Selected lines do not match expected_content; read the file again".into());
        }
    }

    let newline = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut result = lines[..start - 1].concat();
    if !content.is_empty() {
        let mut replacement: Vec<&str> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        if content.ends_with('\n') {
            replacement.pop();
        }
        result.push_str(&replacement.join(newline));
        if end < lines.len() || lines[end - 1].ends_with('\n') {
            result.push_str(newline);
        }
    }
    result.push_str(&lines[end..].concat());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_first_middle_and_last_lines() {
        assert_eq!(
            replace_range("a\nb\nc\n", 1, 1, "A", None).unwrap(),
            "A\nb\nc\n"
        );
        assert_eq!(
            replace_range("a\nb\nc\n", 2, 2, "B\nD", None).unwrap(),
            "a\nB\nD\nc\n"
        );
        assert_eq!(
            replace_range("a\nb\nc", 3, 3, "C\n", None).unwrap(),
            "a\nb\nC"
        );
    }

    #[test]
    fn deletes_selected_lines() {
        assert_eq!(replace_range("a\nb\nc", 2, 2, "", None).unwrap(), "a\nc");
        assert_eq!(replace_range("a\nb\n", 1, 2, "", None).unwrap(), "");
    }

    #[test]
    fn preserves_crlf_and_terminal_newline() {
        assert_eq!(
            replace_range("a\r\nb\r\nc\r\n", 2, 2, "B\nD\n", Some("b")).unwrap(),
            "a\r\nB\r\nD\r\nc\r\n"
        );
        assert_eq!(replace_range("a\n", 1, 1, "A", None).unwrap(), "A\n");
    }

    #[test]
    fn rejects_invalid_ranges_and_stale_content() {
        for (start, end) in [(0, 1), (2, 1), (1, 3)] {
            assert!(replace_range("a\nb", start, end, "x", None).is_err());
        }
        assert!(replace_range("", 1, 1, "x", None).is_err());
        assert!(replace_range("a\nb", 2, 2, "B", Some("old b")).is_err());
        assert_eq!(replace_range("a\nb", 1, 2, "x", Some("a\nb")).unwrap(), "x");
    }

    #[tokio::test]
    async fn edits_file_and_rejects_stale_or_invalid_ranges() {
        let path =
            std::env::temp_dir().join(format!("ozma-replace-lines-{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, "one\ntwo\n").await.unwrap();
        let args = json!({"path": path, "start_line": 2, "end_line": 2, "content": "TWO", "expected_content": "two"});
        assert!(
            ReplaceLines
                .execute(&args, CancellationToken::new())
                .await
                .is_ok()
        );
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "one\nTWO\n"
        );
        assert!(
            !ReplaceLines
                .execute(&args, CancellationToken::new())
                .await
                .is_ok()
        );
        let args = json!({"path": path, "start_line": 3, "end_line": 3, "content": "three"});
        assert!(
            !ReplaceLines
                .execute(&args, CancellationToken::new())
                .await
                .is_ok()
        );
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "one\nTWO\n"
        );
        tokio::fs::remove_file(&path).await.unwrap();
    }
}
