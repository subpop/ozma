use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{PermissionLevel, ToolDef, ToolResult};

/// Fetch the content of a URL.
pub struct WebFetch;

/// Maximum response body size in bytes.
const MAX_BODY_BYTES: usize = 100_000;

#[async_trait]
impl ToolDef for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL and return it as text. \
         Useful for reading web pages, API documentation, or downloading text content. \
         Returns the raw text content (HTML, JSON, etc.) of the response. \
         HTTP URLs are automatically upgraded to HTTPS."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30, max: 120)"
                }
            },
            "required": ["url"]
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    async fn execute(&self, args: &Value, _cancel: CancellationToken) -> ToolResult {
        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return ToolResult::Err("Missing required parameter: url".into()),
        };

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .min(120);

        // Upgrade HTTP to HTTPS
        let url = if url.starts_with("http://") {
            url.replacen("http://", "https://", 1)
        } else {
            url.to_string()
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .user_agent("ozma/0.1")
            .build();

        let client = match client {
            Ok(c) => c,
            Err(e) => return ToolResult::Err(format!("Failed to create HTTP client: {}", e)),
        };

        match client.get(&url).send().await {
            Ok(response) => {
                let status = response.status();
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unknown")
                    .to_string();

                match response.text().await {
                    Ok(body) => {
                        let truncated = body.len() > MAX_BODY_BYTES;
                        let body = if truncated {
                            // Find a char boundary near the limit
                            let mut end = MAX_BODY_BYTES;
                            while end > 0 && !body.is_char_boundary(end) {
                                end -= 1;
                            }
                            format!(
                                "{}\n\n[Truncated: showing first {} of {} bytes]",
                                &body[..end],
                                end,
                                body.len()
                            )
                        } else {
                            body
                        };

                        if status.is_success() {
                            ToolResult::ok_text(format!(
                                "URL: {}\nStatus: {}\nContent-Type: {}\n\n{}",
                                url, status, content_type, body
                            ))
                        } else {
                            ToolResult::Err(format!(
                                "URL: {}\nStatus: {}\nContent-Type: {}\n\n{}",
                                url, status, content_type, body
                            ))
                        }
                    }
                    Err(e) => ToolResult::Err(format!("Failed to read response body: {}", e)),
                }
            }
            Err(e) => ToolResult::Err(format!("Request failed for '{}': {}", url, e)),
        }
    }

    fn display_summary(&self, args: &Value) -> String {
        let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("?");
        // Truncate long URLs for display
        let display = if url.len() > 60 {
            format!("{}...", &url[..57])
        } else {
            url.to_string()
        };
        format!("web_fetch: {}", display)
    }
}
