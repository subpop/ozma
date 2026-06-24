use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{PermissionLevel, ToolDef, ToolResult};

/// Search the web using a search engine.
///
/// Uses the Google Custom Search JSON API if GOOGLE_SEARCH_API_KEY and
/// GOOGLE_SEARCH_CX are set, otherwise falls back to a DuckDuckGo HTML scrape.
pub struct WebSearch;

#[async_trait]
impl ToolDef for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for information. Returns a list of search results with titles, \
         URLs, and snippets. Use this to find up-to-date information, documentation, \
         or answers to questions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5, max: 10)"
                }
            },
            "required": ["query"]
        })
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    async fn execute(&self, args: &Value, _cancel: CancellationToken) -> ToolResult {
        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::Err("Missing required parameter: query".into()),
        };

        let num_results = args
            .get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;

        // Try Google Custom Search if credentials are available
        let api_key = std::env::var("GOOGLE_SEARCH_API_KEY").ok();
        let cx = std::env::var("GOOGLE_SEARCH_CX").ok();

        if let (Some(api_key), Some(cx)) = (api_key, cx) {
            return self.google_search(query, num_results, &api_key, &cx).await;
        }

        // Fallback: DuckDuckGo Lite
        self.ddg_search(query, num_results).await
    }

    fn display_summary(&self, args: &Value) -> String {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("?");
        let display = if query.len() > 50 {
            format!("{}...", &query[..47])
        } else {
            query.to_string()
        };
        format!("web_search: \"{}\"", display)
    }
}

impl WebSearch {
    async fn google_search(
        &self,
        query: &str,
        num_results: usize,
        api_key: &str,
        cx: &str,
    ) -> ToolResult {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build();

        let client = match client {
            Ok(c) => c,
            Err(e) => return ToolResult::Err(format!("Failed to create HTTP client: {}", e)),
        };

        let resp = client
            .get("https://www.googleapis.com/customsearch/v1")
            .query(&[
                ("key", api_key),
                ("cx", cx),
                ("q", query),
                ("num", &num_results.to_string()),
            ])
            .send()
            .await;

        match resp {
            Ok(response) => {
                if !response.status().is_success() {
                    return ToolResult::Err(format!(
                        "Google Search API returned status: {}",
                        response.status()
                    ));
                }

                match response.json::<Value>().await {
                    Ok(json) => {
                        let items = json
                            .get("items")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();

                        if items.is_empty() {
                            return ToolResult::ok_text(format!("No results found for: {}", query));
                        }

                        let results: Vec<String> = items
                            .iter()
                            .enumerate()
                            .map(|(i, item)| {
                                let title =
                                    item.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                                let link = item.get("link").and_then(|v| v.as_str()).unwrap_or("?");
                                let snippet =
                                    item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                                format!("{}. {}\n   {}\n   {}", i + 1, title, link, snippet)
                            })
                            .collect();

                        ToolResult::ok_text(results.join("\n\n"))
                    }
                    Err(e) => ToolResult::Err(format!("Failed to parse search response: {}", e)),
                }
            }
            Err(e) => ToolResult::Err(format!("Search request failed: {}", e)),
        }
    }

    async fn ddg_search(&self, query: &str, num_results: usize) -> ToolResult {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("ozma/0.1")
            .build();

        let client = match client {
            Ok(c) => c,
            Err(e) => return ToolResult::Err(format!("Failed to create HTTP client: {}", e)),
        };

        // Use DuckDuckGo Lite (HTML version)
        let resp = client
            .get("https://lite.duckduckgo.com/lite/")
            .query(&[("q", query)])
            .send()
            .await;

        match resp {
            Ok(response) => {
                let body = match response.text().await {
                    Ok(b) => b,
                    Err(e) => return ToolResult::Err(format!("Failed to read response: {}", e)),
                };

                // Simple HTML parsing for DuckDuckGo Lite results
                let results = parse_ddg_lite(&body, num_results);

                if results.is_empty() {
                    ToolResult::ok_text(format!("No results found for: {}", query))
                } else {
                    ToolResult::ok_text(results.join("\n\n"))
                }
            }
            Err(e) => ToolResult::Err(format!("Search request failed: {}", e)),
        }
    }
}

/// Basic HTML parsing for DuckDuckGo Lite results.
/// Extracts links and snippets from the result table.
fn parse_ddg_lite(html: &str, max_results: usize) -> Vec<String> {
    let mut results = Vec::new();
    let mut i = 0;

    // DuckDuckGo Lite returns results in <a> tags with class="result-link"
    // or in table rows. We look for href patterns in result links.
    for line in html.lines() {
        if results.len() >= max_results {
            break;
        }

        let line = line.trim();

        // Look for result links: <a rel="nofollow" href="URL" class="result-link">Title</a>
        if line.contains("result-link") || line.contains("result__a") {
            if let (Some(href_start), Some(title)) =
                (extract_attr(line, "href"), extract_text_content(line))
            {
                if !href_start.is_empty() && !title.is_empty() {
                    i += 1;
                    results.push(format!("{}. {}\n   {}", i, title, href_start));
                }
            }
        }
    }

    // If the link-based parsing didn't work, try a simpler approach
    if results.is_empty() {
        for line in html.lines() {
            if results.len() >= max_results {
                break;
            }
            let line = line.trim();
            if let Some(url) = extract_attr(line, "href") {
                if url.starts_with("http") && !url.contains("duckduckgo.com") {
                    if let Some(title) = extract_text_content(line) {
                        if !title.is_empty() {
                            i += 1;
                            results.push(format!("{}. {}\n   {}", i, title, url));
                        }
                    }
                }
            }
        }
    }

    results
}

/// Extract an attribute value from an HTML tag.
fn extract_attr<'a>(html: &'a str, attr: &str) -> Option<&'a str> {
    let pattern = format!("{}=\"", attr);
    let start = html.find(&pattern)? + pattern.len();
    let end = html[start..].find('"')? + start;
    Some(&html[start..end])
}

/// Extract text content from between HTML tags (very basic).
fn extract_text_content(html: &str) -> Option<String> {
    // Find content between > and <
    let start = html.find('>')? + 1;
    let rest = &html[start..];
    let end = rest.find('<').unwrap_or(rest.len());
    let text = rest[..end].trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_attr_and_text() {
        let tag = r#"<a href="https://example.com" class="result-link">Example</a>"#;
        assert_eq!(extract_attr(tag, "href"), Some("https://example.com"));
        assert_eq!(extract_text_content(tag).as_deref(), Some("Example"));
        assert!(extract_text_content("<a></a>").is_none());
    }

    #[test]
    fn parse_ddg_lite_result_link() {
        let html = r#"
<html>
<a rel="nofollow" href="https://rust-lang.org" class="result-link">Rust</a>
<a rel="nofollow" href="https://doc.rust-lang.org" class="result-link">Docs</a>
</html>
"#;
        let results = parse_ddg_lite(html, 1);
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("Rust"));
        assert!(results[0].contains("https://rust-lang.org"));
    }

    #[test]
    fn parse_ddg_lite_fallback_http_links() {
        let html = r#"
<a href="https://example.com/page">Fallback Title</a>
<a href="https://duckduckgo.com/x">Skip</a>
"#;
        let results = parse_ddg_lite(html, 5);
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("Fallback Title"));
        assert!(results[0].contains("https://example.com/page"));
    }
}
