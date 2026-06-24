//! Convert ozma MCP config into ACP `mcpServers` for session/new|load.

use agent_client_protocol::schema::v1::{EnvVariable, McpServer, McpServerHttp, McpServerStdio};

use crate::config::McpServerConfig;

/// Build ACP MCP server descriptors from ozma config.
///
/// Stdio servers are always included. HTTP servers are included only when
/// `include_http` is true (agent advertised `mcpCapabilities.http`).
pub fn mcp_servers_from_config(configs: &[McpServerConfig], include_http: bool) -> Vec<McpServer> {
    let mut out = Vec::new();
    for cfg in configs {
        match cfg.transport.as_str() {
            "http" => {
                if !include_http {
                    continue;
                }
                let Some(url) = cfg.url.as_ref() else {
                    continue;
                };
                out.push(McpServer::Http(McpServerHttp::new(cfg.name.clone(), url)));
            }
            _ => {
                let Some(command) = cfg.command.as_ref() else {
                    continue;
                };
                let env: Vec<EnvVariable> = cfg
                    .env
                    .iter()
                    .map(|(k, v)| EnvVariable::new(k.clone(), v.clone()))
                    .collect();
                out.push(McpServer::Stdio(
                    McpServerStdio::new(cfg.name.clone(), command)
                        .args(cfg.args.clone())
                        .env(env),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn stdio_cfg(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".into(), "server".into()],
            env: HashMap::new(),
            url: None,
        }
    }

    fn http_cfg(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: "http".to_string(),
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: Some("http://localhost:3000/mcp".to_string()),
        }
    }

    #[test]
    fn stdio_always_included() {
        let servers = mcp_servers_from_config(&[stdio_cfg("fs")], false);
        assert_eq!(servers.len(), 1);
        assert!(matches!(servers[0], McpServer::Stdio(_)));
    }

    #[test]
    fn http_skipped_without_capability() {
        let servers = mcp_servers_from_config(&[stdio_cfg("fs"), http_cfg("remote")], false);
        assert_eq!(servers.len(), 1);
    }

    #[test]
    fn http_included_with_capability() {
        let servers = mcp_servers_from_config(&[stdio_cfg("fs"), http_cfg("remote")], true);
        assert_eq!(servers.len(), 2);
        assert!(matches!(servers[1], McpServer::Http(_)));
    }
}
