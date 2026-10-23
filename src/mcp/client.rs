use color_eyre::eyre::{Result, eyre};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult, Tool as McpTool},
    service::RunningServiceCancellationToken,
    transport::TokioChildProcess,
};
use tokio::process::Command;
use tracing::{debug, info};

use crate::config::McpServerConfig;

/// A connected MCP server client. Wraps an rmcp `RunningService`.
pub struct McpClient {
    /// The display name of this MCP server.
    pub name: String,
    /// The peer handle for making RPC calls to the server.
    peer: rmcp::service::Peer<rmcp::service::RoleClient>,
    /// Cancellation token for graceful shutdown.
    cancellation_token: Option<RunningServiceCancellationToken>,
    /// Tools discovered from this server (cached after list_tools).
    pub tools: Vec<McpTool>,
}

impl McpClient {
    /// Connect to an MCP server using the given config.
    /// Performs the initialization handshake and lists available tools.
    pub async fn connect(config: &McpServerConfig) -> Result<Self> {
        match config.transport.as_str() {
            "stdio" => Self::connect_stdio(config).await,
            "http" => Self::connect_http(config).await,
            other => Err(eyre!("Unknown MCP transport: {}", other)),
        }
    }

    /// Connect to an MCP server via stdio (child process) transport.
    async fn connect_stdio(config: &McpServerConfig) -> Result<Self> {
        let command = config.command.as_ref().ok_or_else(|| {
            eyre!(
                "MCP server '{}': 'command' is required for stdio transport",
                config.name
            )
        })?;

        info!(
            "Connecting to MCP server '{}' via stdio: {} {:?}",
            config.name, command, config.args
        );

        let mut cmd = Command::new(command);
        cmd.args(&config.args);

        // Set environment variables
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| eyre!("Failed to spawn MCP server '{}': {}", config.name, e))?;

        let service = ()
            .serve(transport)
            .await
            .map_err(|e| eyre!("Failed to initialize MCP server '{}': {}", config.name, e))?;

        let peer = service.peer().clone();
        let cancellation_token = Some(service.cancellation_token());

        // Get server info
        if let Some(server_info) = service
            .peer_info()
            .as_deref()
            .and_then(|info| info.server_info.as_ref())
        {
            info!(
                "MCP server '{}' connected: {:?} {:?}",
                config.name, server_info.name, server_info.version,
            );
        }

        // List available tools
        let tools = peer.list_all_tools().await.map_err(|e| {
            eyre!(
                "Failed to list tools from MCP server '{}': {}",
                config.name,
                e
            )
        })?;

        info!(
            "MCP server '{}': {} tool(s) available",
            config.name,
            tools.len()
        );
        for tool in &tools {
            debug!(
                "  MCP tool: {} — {}",
                tool.name,
                tool.description.as_deref().unwrap_or("(no description)")
            );
        }

        Ok(Self {
            name: config.name.clone(),
            peer,
            cancellation_token,
            tools,
        })
    }

    /// Connect to an MCP server via HTTP transport.
    async fn connect_http(config: &McpServerConfig) -> Result<Self> {
        let url = config.url.as_ref().ok_or_else(|| {
            eyre!(
                "MCP server '{}': 'url' is required for http transport",
                config.name
            )
        })?;

        info!(
            "Connecting to MCP server '{}' via HTTP: {}",
            config.name, url
        );

        let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(url.as_str());

        let service = ()
            .serve(transport)
            .await
            .map_err(|e| eyre!("Failed to initialize MCP server '{}': {}", config.name, e))?;

        let peer = service.peer().clone();
        let cancellation_token = Some(service.cancellation_token());

        if let Some(server_info) = service
            .peer_info()
            .as_deref()
            .and_then(|info| info.server_info.as_ref())
        {
            info!(
                "MCP server '{}' connected: {:?} {:?}",
                config.name, server_info.name, server_info.version,
            );
        }

        let tools = peer.list_all_tools().await.map_err(|e| {
            eyre!(
                "Failed to list tools from MCP server '{}': {}",
                config.name,
                e
            )
        })?;

        info!(
            "MCP server '{}': {} tool(s) available",
            config.name,
            tools.len()
        );

        Ok(Self {
            name: config.name.clone(),
            peer,
            cancellation_token,
            tools,
        })
    }

    /// Call a tool on this MCP server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        debug!("Calling MCP tool '{}' on server '{}'", tool_name, self.name);

        // Build arguments map from the JSON value
        let tool_name_owned = tool_name.to_string();
        let params = match arguments {
            serde_json::Value::Object(map) => {
                CallToolRequestParams::new(tool_name_owned).with_arguments(map)
            }
            serde_json::Value::Null => CallToolRequestParams::new(tool_name_owned),
            _ => {
                return Err(eyre!(
                    "MCP tool arguments must be a JSON object, got: {}",
                    arguments
                ));
            }
        };

        let result = self
            .peer
            .call_tool(params)
            .await
            .map_err(|e| eyre!("MCP tool '{}' call failed: {}", tool_name, e))?;

        debug!(
            "MCP tool '{}' returned: is_error={:?}, {} content block(s)",
            tool_name,
            result.is_error,
            result.content.len()
        );

        Ok(result)
    }

    /// Gracefully shut down this MCP server connection.
    pub async fn shutdown(self) {
        info!("Shutting down MCP server '{}'", self.name);
        if let Some(token) = self.cancellation_token {
            token.cancel();
        }
    }
}
