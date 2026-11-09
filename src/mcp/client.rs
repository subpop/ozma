use color_eyre::eyre::{Result, eyre};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult, Tool as McpTool},
    service::{RoleClient, RunningService},
    transport::{IntoTransport, TokioChildProcess},
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::config::McpServerConfig;
use crate::event::Event;

/// Cap the length of a single retained stderr line. Some servers emit very
/// long lines (stack traces, JSON dumps); keep memory usage bounded.
const MAX_LINE_LEN: usize = 4096;

/// A connected MCP server client. Owns the rmcp `RunningService`.
pub struct McpClient {
    /// The display name of this MCP server.
    pub name: String,
    /// The running service behind the connection. Must be owned for as long
    /// as the client lives: dropping a `RunningService` cancels its
    /// transport, after which every request fails with "Transport closed".
    service: RunningService<RoleClient, ()>,
    /// Tools discovered from this server (cached after list_tools).
    pub tools: Vec<McpTool>,
}

impl McpClient {
    /// Connect to an MCP server using the given config.
    /// Performs the initialization handshake and lists available tools.
    /// `event_tx` receives captured stderr lines as `Event::McpLog`.
    pub async fn connect(
        config: &McpServerConfig,
        event_tx: &UnboundedSender<Event>,
    ) -> Result<Self> {
        match config.transport.as_str() {
            "stdio" => Self::connect_stdio(config, event_tx).await,
            "http" => Self::connect_http(config).await,
            other => Err(eyre!("Unknown MCP transport: {}", other)),
        }
    }

    /// Connect to an MCP server via stdio (child process) transport.
    /// The child's stderr is piped (not inherited) so server output cannot
    /// corrupt the TUI; lines are logged and forwarded as `Event::McpLog`.
    async fn connect_stdio(
        config: &McpServerConfig,
        event_tx: &UnboundedSender<Event>,
    ) -> Result<Self> {
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

        let (transport, stderr) = TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| eyre!("Failed to spawn MCP server '{}': {}", config.name, e))?;

        if let Some(stderr) = stderr {
            let name = config.name.clone();
            let event_tx = event_tx.clone();
            tokio::spawn(forward_stderr(name, stderr, event_tx));
        }

        Self::connect_with_transport(&config.name, transport).await
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

        Self::connect_with_transport(&config.name, transport).await
    }

    /// Connect over any rmcp transport and run the common post-handshake
    /// setup (server info, tool discovery). Split out from the
    /// transport-specific constructors so tests can inject an in-memory
    /// transport.
    async fn connect_with_transport<T, E, A>(name: &str, transport: T) -> Result<Self>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service = ()
            .serve(transport)
            .await
            .map_err(|e| eyre!("Failed to initialize MCP server '{}': {}", name, e))?;

        // Get server info
        if let Some(server_info) = service
            .peer_info()
            .as_deref()
            .and_then(|info| info.server_info.as_ref())
        {
            info!(
                "MCP server '{}' connected: {:?} {:?}",
                name, server_info.name, server_info.version,
            );
        }

        // List available tools
        let tools = service
            .list_all_tools()
            .await
            .map_err(|e| eyre!("Failed to list tools from MCP server '{}': {}", name, e))?;

        info!("MCP server '{}': {} tool(s) available", name, tools.len());
        for tool in &tools {
            debug!(
                "  MCP tool: {} — {}",
                tool.name,
                tool.description.as_deref().unwrap_or("(no description)")
            );
        }

        // Keep `service` alive in the client: dropping it here would cancel
        // the transport and make every later call_tool fail with
        // "Transport closed".
        Ok(Self {
            name: name.to_string(),
            service,
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
            .service
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
        // Cancel the service and wait for the transport to close, so the
        // server process (stdio) is cleaned up before we return.
        let _ = self.service.cancel().await;
    }
}

/// Read a stdio MCP server's stderr to EOF, logging every line and
/// forwarding it as `Event::McpLog`. Empty lines are skipped and overly
/// long lines are truncated so a chatty or pathological server cannot
/// grow memory without bound.
async fn forward_stderr<S>(name: String, mut stderr: S, event_tx: UnboundedSender<Event>)
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(&mut stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break, // EOF or error: server went away
            Ok(_) => {
                let text = line.trim_end_matches(['\n', '\r']);
                let text = if text.len() > MAX_LINE_LEN {
                    // Byte-slice at a char boundary to avoid panicking
                    // on multi-byte UTF-8 stderr output.
                    let mut end = MAX_LINE_LEN;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    warn!(
                        "[mcp:{}] stderr line truncated ({} bytes)",
                        name,
                        text.len()
                    );
                    &text[..end]
                } else {
                    text
                };
                if !text.is_empty() {
                    info!("[mcp:{}] {}", name, text);
                    let _ = event_tx.send(Event::McpLog {
                        server: name.clone(),
                        line: text.to_string(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{
        CallToolResult, ContentBlock, Implementation, InitializeResult, ListToolsResult,
        ProtocolVersion, ServerCapabilities, ServerJsonRpcMessage, ServerResult, Tool,
    };
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// End-to-end: `connect_stdio` must pipe (not inherit) the child's
    /// stderr. Here the "server" is a shell that writes a stderr line and
    /// exits without speaking MCP — the handshake fails, but the line must
    /// still be captured as an event rather than leaking to the terminal.
    #[tokio::test]
    async fn connect_stdio_pipes_child_stderr() {
        let (event_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let cfg = crate::config::McpServerConfig {
            name: "noisy".to_string(),
            transport: "stdio".to_string(),
            command: Some("sh".to_string()),
            args: vec!["-c".to_string(), "echo noisy-stderr-line >&2".to_string()],
            env: std::collections::HashMap::new(),
            url: None,
        };

        // The handshake fails (the child speaks no MCP) — that's expected;
        // we only care that its stderr was captured along the way.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            McpClient::connect(&cfg, &event_tx),
        )
        .await
        .expect("connect should terminate (handshake failure)");
        assert!(
            result.is_err(),
            "handshake with a non-MCP process must fail"
        );

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("captured stderr line should arrive")
            .expect("reader task should still be alive");
        match event {
            Event::McpLog { server, line } => {
                assert_eq!(server, "noisy");
                assert_eq!(line, "noisy-stderr-line");
            }
            other => panic!("expected Event::McpLog, got {other:?}"),
        }
    }

    /// Regression test: stdio MCP servers used to inherit the TUI's stderr,
    /// so their output printed over the ratatui display. `forward_stderr`
    /// now captures lines and forwards them as `Event::McpLog` instead.
    #[tokio::test]
    async fn stdio_stderr_is_captured_as_events() {
        let (event_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let (mut writer, reader) = tokio::io::duplex(8192);

        let task = tokio::spawn(forward_stderr("srv".to_string(), reader, event_tx));

        // A normal line, an empty line (skipped), and an over-long line
        // (truncated to MAX_LINE_LEN at a char boundary).
        writer.write_all(b"hello from stderr\n").await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer
            .write_all(&vec![b'x'; MAX_LINE_LEN + 100])
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        drop(writer); // EOF: server went away

        match rx.recv().await.unwrap() {
            Event::McpLog { server, line } => {
                assert_eq!(server, "srv");
                assert_eq!(line, "hello from stderr");
            }
            other => panic!("expected Event::McpLog, got {other:?}"),
        }
        match rx.recv().await.unwrap() {
            Event::McpLog { line, .. } => assert_eq!(line.len(), MAX_LINE_LEN),
            other => panic!("expected Event::McpLog, got {other:?}"),
        }

        // EOF terminates the reader task and no further events arrive.
        task.await.unwrap();
        assert!(rx.recv().await.is_none());
    }

    /// Regression test: `McpClient` used to clone the `Peer` out of the rmcp
    /// `RunningService` and drop the service at the end of `connect`. The
    /// service's drop guard cancelled the transport, so every tool call made
    /// after connect failed immediately with "Transport closed". Owning the
    /// `RunningService` for the client's lifetime fixes it.
    #[tokio::test]
    async fn call_tool_works_after_connect() {
        let (server_io, client_io) = tokio::io::duplex(4096);
        tokio::spawn(mock_mcp_server(server_io));

        let client = McpClient::connect_with_transport("mock", client_io)
            .await
            .expect("connect should complete the handshake and list tools");

        // Tool discovery ran before the service was dropped, so it worked
        // even with the bug — assert it anyway to pin the full handshake.
        assert_eq!(client.tools.len(), 1);
        assert_eq!(client.tools[0].name, "echo");

        // The actual regression: the first tool call after connect used to
        // fail with "Transport closed".
        let result = client
            .call_tool("echo", json!({"message": "hello"}))
            .await
            .expect("tool call after connect must not fail with Transport closed");
        assert_eq!(result.is_error, Some(false));
        assert_eq!(only_text(&result), "echo: hello");

        // A second call proves the connection stays healthy beyond the first.
        let result = client
            .call_tool("echo", json!({"message": "again"}))
            .await
            .expect("second tool call must succeed");
        assert_eq!(only_text(&result), "echo: again");

        client.shutdown().await;
    }

    fn only_text(result: &CallToolResult) -> &str {
        match result.content.first() {
            Some(ContentBlock::Text(text)) => &text.text,
            other => panic!("expected a single text content block, got {other:?}"),
        }
    }

    /// Minimal newline-delimited JSON-RPC MCP server: answers `initialize`,
    /// `tools/list` (one `echo` tool) and `tools/call` (echoes
    /// `params.arguments.message` back as text).
    async fn mock_mcp_server<S>(io: S)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (reader, mut writer) = tokio::io::split(io);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break, // EOF or error: client went away
                Ok(_) => {}
            }
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            // Notifications (e.g. `notifications/initialized`) carry no id and
            // need no reply.
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let result = match method {
                "initialize" => ServerResult::InitializeResult(
                    InitializeResult::new(ServerCapabilities::default())
                        .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
                        .with_server_info(Implementation::new("mock-mcp", "0.0.0")),
                ),
                "tools/list" => ServerResult::ListToolsResult(ListToolsResult::with_all_items(
                    vec![echo_tool()],
                )),
                "tools/call" => {
                    let message = message
                        .pointer("/params/arguments/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    ServerResult::CallToolResult(CallToolResult::success(vec![ContentBlock::text(
                        format!("echo: {message}"),
                    )]))
                }
                _ => continue,
            };
            let id: rmcp::model::RequestId = serde_json::from_value(id).unwrap();
            let response = ServerJsonRpcMessage::response(result, id);
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            writer.write_all(&bytes).await.unwrap();
        }
    }

    fn echo_tool() -> Tool {
        let schema = json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        });
        Tool::new(
            "echo",
            "Echo the message back",
            std::sync::Arc::new(schema.as_object().expect("object schema").clone()),
        )
    }
}
