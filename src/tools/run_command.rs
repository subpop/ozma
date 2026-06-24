use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{PermissionLevel, ToolDef, ToolResult};

/// Execute a shell command and return its output.
pub struct RunCommand;

/// Maximum output size in bytes before truncation.
const MAX_OUTPUT_BYTES: usize = 50_000;

#[async_trait]
impl ToolDef for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its stdout and stderr output. \
         The command runs in the system's default shell. \
         Use this for running build tools, tests, git commands, or other CLI operations. \
         Commands have a default timeout of 30 seconds."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "working_directory": {
                    "type": "string",
                    "description": "The working directory to run the command in (optional, defaults to current directory)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30, max: 300)"
                }
            },
            "required": ["command"]
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Dangerous
    }

    async fn execute(&self, args: &Value, cancel: CancellationToken) -> ToolResult {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::Err("Missing required parameter: command".into()),
        };

        let working_dir = args.get("working_directory").and_then(|v| v.as_str());

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .min(300);

        // Determine shell
        let shell = if cfg!(target_os = "windows") {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };

        let mut cmd = Command::new(shell.0);
        cmd.arg(shell.1).arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Pipe stdout/stderr so we can capture them
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Spawn the child process so we can kill it on cancellation
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolResult::Err(format!("Failed to execute command: {}", e)),
        };

        let timeout = std::time::Duration::from_secs(timeout_secs);

        tokio::select! {
            result = tokio::time::timeout(timeout, child.wait()) => {
                match result {
                    Ok(Ok(status)) => {
                        // Read captured output after process exits
                        let mut stdout_bytes = Vec::new();
                        let mut stderr_bytes = Vec::new();
                        if let Some(mut out) = child.stdout.take() {
                            let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut stdout_bytes).await;
                        }
                        if let Some(mut err) = child.stderr.take() {
                            let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut stderr_bytes).await;
                        }

                        let mut parts = Vec::new();
                        let stdout = String::from_utf8_lossy(&stdout_bytes);
                        let stderr = String::from_utf8_lossy(&stderr_bytes);

                        if !stdout.is_empty() {
                            parts.push(format!("STDOUT:\n{}", truncate(&stdout, MAX_OUTPUT_BYTES)));
                        }
                        if !stderr.is_empty() {
                            parts.push(format!("STDERR:\n{}", truncate(&stderr, MAX_OUTPUT_BYTES)));
                        }

                        let exit_code = status.code().unwrap_or(-1);
                        parts.push(format!("Exit code: {}", exit_code));

                        let content = parts.join("\n\n");
                        if status.success() {
                            ToolResult::ok_text(content)
                        } else {
                            ToolResult::Err(content)
                        }
                    }
                    Ok(Err(e)) => ToolResult::Err(format!("Failed to execute command: {}", e)),
                    Err(_) => {
                        // Timeout — kill the child process
                        let _ = child.kill().await;
                        ToolResult::Err(format!(
                            "Command timed out after {} seconds",
                            timeout_secs
                        ))
                    }
                }
            }
            _ = cancel.cancelled() => {
                debug!("Cancelling running command: {}", command);
                let _ = child.kill().await;
                ToolResult::Err("Command cancelled (application shutting down)".into())
            }
        }
    }

    fn display_summary(&self, args: &Value) -> String {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("?");
        // Truncate long commands for display
        let display = if command.len() > 60 {
            format!("{}...", &command[..57])
        } else {
            command.to_string()
        };
        format!("run_command: {}", display)
    }
}

fn truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        s
    } else {
        // Find a char boundary near max_bytes
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_ascii_boundary() {
        assert_eq!(truncate("abcdef", 3), "abc");
    }

    #[test]
    fn truncate_respects_utf8_char_boundary() {
        // "é" is 2 bytes; max_bytes=3 would land mid-char on "café" (c,a,f,é)
        let s = "café!";
        assert_eq!(truncate(s, 4), "caf"); // byte 4 is mid-é, back up to 3
        assert_eq!(truncate(s, 5), "café");
    }
}
