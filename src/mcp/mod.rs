pub mod client;

use std::collections::HashMap;

use base64::Engine;
use genai::chat::{Tool as GenaiTool, ToolName};
use rmcp::model::ContentBlock;
use serde_json::Value;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::McpServerConfig;
use crate::model::message::ContentPart;

use self::client::McpClient;

/// Output from an MCP tool execution that may contain binary blobs
/// (images) alongside structured content parts.
pub struct McpToolOutput {
    /// Structured content parts for the message model.
    pub parts: Vec<ContentPart>,
    /// Binary blobs to persist as attachments: (id, mime_type, data).
    pub blobs: Vec<(Uuid, String, Vec<u8>)>,
    /// Whether the MCP server reported an error.
    pub is_error: bool,
}

/// Manages all connected MCP servers and provides tool resolution/execution.
///
/// Each MCP server's tools are namespaced as `server_name__tool_name` to avoid
/// collisions between servers and with built-in tools.
pub struct McpManager {
    /// Connected MCP server clients, keyed by server name.
    clients: HashMap<String, McpClient>,
    /// Map from namespaced tool name → (server_name, original_tool_name).
    tool_map: HashMap<String, (String, String)>,
}

impl McpManager {
    /// Create an empty manager with no servers connected.
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tool_map: HashMap::new(),
        }
    }

    /// Connect to all MCP servers defined in the config.
    /// Errors for individual servers are logged but don't prevent other servers from connecting.
    pub async fn connect_all(&mut self, configs: &[McpServerConfig]) {
        for config in configs {
            match McpClient::connect(config).await {
                Ok(client) => {
                    // Register all tools from this server
                    for tool in &client.tools {
                        let namespaced = format!("{}__{}", client.name, tool.name);
                        self.tool_map.insert(
                            namespaced.clone(),
                            (client.name.clone(), tool.name.to_string()),
                        );
                        info!(
                            "Registered MCP tool: {} (from server '{}')",
                            namespaced, client.name
                        );
                    }
                    self.clients.insert(client.name.clone(), client);
                }
                Err(e) => {
                    error!("Failed to connect to MCP server '{}': {}", config.name, e);
                }
            }
        }
    }

    /// Whether this manager has any tools registered.
    #[allow(dead_code)]
    pub fn has_tools(&self) -> bool {
        !self.tool_map.is_empty()
    }

    /// Get the number of connected servers.
    #[allow(dead_code)]
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// Get the total number of MCP tools available.
    #[allow(dead_code)]
    pub fn tool_count(&self) -> usize {
        self.tool_map.len()
    }

    /// Check if a tool name is an MCP tool (i.e., matches a namespaced tool name).
    pub fn is_mcp_tool(&self, name: &str) -> bool {
        self.tool_map.contains_key(name)
    }

    /// Generate genai `Tool` definitions for all MCP tools.
    /// Tool names are namespaced as `server_name__tool_name`.
    pub fn genai_tools(&self) -> Vec<GenaiTool> {
        let mut tools = Vec::new();

        for client in self.clients.values() {
            for tool in &client.tools {
                let namespaced_name = format!("{}__{}", client.name, tool.name);

                let mut genai_tool = GenaiTool::new(ToolName::Custom(namespaced_name))
                    .with_schema(tool.schema_as_json_value());

                if let Some(desc) = tool.description.as_deref() {
                    genai_tool = genai_tool.with_description(desc);
                }

                tools.push(genai_tool);
            }
        }

        tools
    }

    /// Execute an MCP tool call.
    /// `namespaced_name` should be in the form `server_name__tool_name`.
    /// Returns `McpToolOutput` with structured content parts and any binary blobs,
    /// or a `ToolResult::Err` on failure.
    pub async fn execute_tool(
        &self,
        namespaced_name: &str,
        arguments: Value,
    ) -> Result<McpToolOutput, String> {
        let Some((server_name, tool_name)) = self.tool_map.get(namespaced_name) else {
            return Err(format!("Unknown MCP tool: {}", namespaced_name));
        };

        let Some(client) = self.clients.get(server_name) else {
            return Err(format!("MCP server '{}' is not connected", server_name));
        };

        match client.call_tool(tool_name, arguments).await {
            Ok(result) => {
                let is_error = result.is_error.unwrap_or(false);
                let (parts, blobs) = extract_content(&result.content);
                Ok(McpToolOutput {
                    parts,
                    blobs,
                    is_error,
                })
            }
            Err(e) => Err(format!("MCP tool execution error: {}", e)),
        }
    }

    /// Gracefully shut down all connected MCP servers.
    pub async fn shutdown(&mut self) {
        for (name, client) in self.clients.drain() {
            info!("Shutting down MCP server: {}", name);
            client.shutdown().await;
        }
        self.tool_map.clear();
    }
}

/// Extract structured content parts from MCP content blocks, preserving images.
/// Returns (content_parts, blobs) where blobs are (id, mime_type, data) tuples
/// ready to be persisted as attachments.
fn extract_content(content: &[ContentBlock]) -> (Vec<ContentPart>, Vec<(Uuid, String, Vec<u8>)>) {
    let mut parts = Vec::new();
    let mut blobs = Vec::new();

    for block in content {
        match block {
            ContentBlock::Text(text_content) => {
                parts.push(ContentPart::Text {
                    text: text_content.text.clone(),
                });
            }
            ContentBlock::Image(img) => {
                // Decode base64 image data from MCP response
                match base64::engine::general_purpose::STANDARD.decode(&img.data) {
                    Ok(data) => {
                        let id = Uuid::new_v4();
                        parts.push(ContentPart::Image {
                            id,
                            mime_type: img.mime_type.clone(),
                            alt: None,
                        });
                        blobs.push((id, img.mime_type.clone(), data));
                    }
                    Err(e) => {
                        warn!("Failed to decode MCP image data: {}", e);
                        parts.push(ContentPart::Text {
                            text: "[image: decode error]".to_string(),
                        });
                    }
                }
            }
            ContentBlock::Audio(_) => {
                parts.push(ContentPart::Text {
                    text: "[audio content]".to_string(),
                });
            }
            ContentBlock::Resource(resource) => match &resource.resource {
                rmcp::model::ResourceContents::TextResourceContents { text, .. } => {
                    parts.push(ContentPart::Text { text: text.clone() });
                }
                _ => {
                    parts.push(ContentPart::Text {
                        text: "[resource content]".to_string(),
                    });
                }
            },
            _ => {
                warn!("Unknown MCP content type, skipping");
            }
        }
    }

    (parts, blobs)
}
