use std::collections::HashMap;
use std::path::PathBuf;

use color_eyre::eyre::{Result, eyre};
use serde::Deserialize;
use tracing::info;

/// Top-level application configuration, loaded from `~/.config/ozma/config.toml`.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// Default source name (provider or agent).
    #[serde(default)]
    pub default_source: Option<String>,

    /// Provider configurations.
    #[serde(default)]
    pub provider: Vec<ProviderConfig>,

    /// ACP agent configurations (external coding agents over stdio).
    #[serde(default)]
    pub agent: Vec<AgentConfig>,

    /// Tool permission settings.
    #[serde(default)]
    pub tools: ToolsConfig,

    /// MCP server configurations.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// The default persona name (e.g., "ozma", "ozcode", "ozreview", "ozplan").
    #[serde(default)]
    pub default_persona: Option<String>,
}

/// Which kind of backend a configured source refers to.
#[derive(Clone, Debug)]
pub enum SourceKind<'a> {
    Provider(&'a ProviderConfig),
    Agent(&'a AgentConfig),
}

/// Configuration for an ACP agent (e.g. `opencode acp`).
#[derive(Clone, Debug, Deserialize)]
pub struct AgentConfig {
    /// Human-readable name (shown in the model/source picker).
    pub name: String,

    /// Command to spawn (e.g. `"opencode"`).
    pub command: String,

    /// Arguments for the command (e.g. `["acp"]`).
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables for the agent process.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Working directory for agent sessions. Defaults to process cwd when unset.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

/// Configuration for tool permissions and behavior.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolsConfig {
    /// Whether to auto-approve read-only tools (read_file, list_directory, web_fetch, web_search).
    /// Default: true.
    #[serde(default = "default_true")]
    pub auto_approve_read: bool,

    /// Whether to auto-approve destructive tools (write_file).
    /// Default: false.
    #[serde(default)]
    pub auto_approve_write: bool,

    /// Whether to auto-approve dangerous tools (run_command).
    /// Default: false.
    #[serde(default)]
    pub auto_approve_execute: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            auto_approve_read: true,
            auto_approve_write: false,
            auto_approve_execute: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Configuration for an MCP (Model Context Protocol) server.
#[derive(Clone, Debug, Deserialize)]
pub struct McpServerConfig {
    /// Human-readable name for this server (e.g., "filesystem", "github").
    pub name: String,

    /// Transport type: "stdio" (default) or "http".
    #[serde(default = "default_stdio")]
    pub transport: String,

    /// The command to run for stdio transport (e.g., "npx", "uvx", "node").
    #[serde(default)]
    pub command: Option<String>,

    /// Arguments for the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set for the child process.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// URL for HTTP transport (e.g., "http://localhost:3000/mcp").
    #[serde(default)]
    pub url: Option<String>,
}

fn default_stdio() -> String {
    "stdio".to_string()
}

/// Default model parameters (temperature, top_p, max_tokens).
///
/// All fields are optional. When set in a provider config, they act as defaults.
/// Per-conversation overrides (session-scoped, not persisted) can further override these.
#[derive(Clone, Debug, Default)]
pub struct ModelParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u32>,
}

impl ModelParams {
    /// Whether any parameter is set.
    pub fn has_any(&self) -> bool {
        self.temperature.is_some() || self.top_p.is_some() || self.max_tokens.is_some()
    }

    /// Merge with overrides: override values take precedence, falling back to self.
    pub fn merged_with(&self, overrides: &ModelParams) -> ModelParams {
        ModelParams {
            temperature: overrides.temperature.or(self.temperature),
            top_p: overrides.top_p.or(self.top_p),
            max_tokens: overrides.max_tokens.or(self.max_tokens),
        }
    }
}

/// Wire protocol override for providers that support multiple protocols behind one endpoint.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    /// Use adapter-native behavior.
    Auto,
    /// OpenAI Chat Completions (`/chat/completions`).
    ChatCompletions,
    /// OpenAI Responses (`/responses`).
    Responses,
    /// Anthropic Messages (`/messages`).
    Messages,
}

/// Configuration for a single LLM provider.
#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    /// Human-readable name for this provider (e.g., "openai", "anthropic", "local-ollama").
    pub name: String,

    /// Available model identifiers (e.g., ["gpt-4o-mini", "gpt-4o", "o3-mini"]).
    /// The first entry is the default model. When omitted, ozma queries the provider
    /// at startup to discover available models.
    #[serde(default)]
    pub models: Option<Vec<String>>,

    /// Environment variable name holding the API key (e.g., "OPENAI_API_KEY").
    /// If not set, genai's default env var resolution is used.
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Shell command to run to retrieve the API key (e.g. a password manager CLI).
    /// The command is run via `sh -c` (`cmd /C` on Windows) on every request, and its
    /// trimmed stdout is used as the key. Mutually exclusive with `api_key_env`.
    #[serde(default)]
    pub api_key_cmd: Option<String>,

    /// Custom base URL for the API endpoint (e.g., "http://localhost:11434/v1/").
    /// Optional custom base URL for the API endpoint (e.g. "http://localhost:11434/v1/").
    /// Overrides the adapter's default endpoint when set.
    #[serde(default)]
    pub base_url: Option<String>,

    /// The protocol adapter to use (e.g. "openai", "anthropic", "gemini", "ollama",
    /// "opencode_go"). Determines the default wire format and endpoint behavior.
    pub adapter: String,

    /// Optional wire protocol override for adapters that expose multiple protocols
    /// behind a single endpoint (e.g. OpenCode Go).
    ///
    /// Supported values:
    /// - "auto" (default behavior)
    /// - "chat_completions"
    /// - "responses"
    /// - "messages"
    #[serde(default)]
    pub wire_protocol: Option<WireProtocol>,

    /// Default temperature for this provider (0.0–2.0).
    #[serde(default)]
    pub temperature: Option<f64>,

    /// Default top_p for this provider (0.0–1.0).
    #[serde(default)]
    pub top_p: Option<f64>,

    /// Default max_tokens for this provider.
    #[serde(default)]
    pub max_tokens: Option<u32>,

    /// Additional HTTP headers sent with every request for this provider.
    /// Merged on top of any adapter-injected headers (e.g. User-Agent, x-opencode-session).
    #[serde(default)]
    pub additional_headers: HashMap<String, String>,
}

impl ProviderConfig {
    /// The default model (first in the explicit models list), if configured.
    pub fn default_model(&self) -> Option<&str> {
        self.models.as_ref()?.first().map(|s| s.as_str())
    }

    /// Extract model parameters from this provider config.
    pub fn model_params(&self) -> ModelParams {
        ModelParams {
            temperature: self.temperature,
            top_p: self.top_p,
            max_tokens: self.max_tokens,
        }
    }
}

impl Config {
    /// Load configuration from `~/.config/ozma/config.toml`.
    /// Returns a default config if the file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;

        if !path.exists() {
            info!("No config file at {:?}, using defaults", path);
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&path)
            .map_err(|e| eyre!("Failed to read config file {:?}: {}", path, e))?;

        let config: Config = toml::from_str(&contents)
            .map_err(|e| eyre!("Failed to parse config file {:?}: {}", path, e))?;

        config.validate()?;

        info!(
            "Loaded config from {:?} ({} providers)",
            path,
            config.provider.len()
        );
        Ok(config)
    }

    /// Validate cross-field constraints that serde can't express on its own.
    fn validate(&self) -> Result<()> {
        for provider in &self.provider {
            if provider.api_key_env.is_some() && provider.api_key_cmd.is_some() {
                return Err(eyre!(
                    "Provider '{}' sets both api_key_env and api_key_cmd; only one may be set",
                    provider.name
                ));
            }
        }
        Ok(())
    }

    /// Get the path to the config file.
    fn config_path() -> Result<PathBuf> {
        let config_dir = config_base_dir()?;
        Ok(config_dir.join("ozma").join("config.toml"))
    }

    /// Default source name: `default_source`, else first provider/agent.
    pub fn default_source_name(&self) -> Option<&str> {
        self.default_source
            .as_deref()
            .or_else(|| self.provider.first().map(|p| p.name.as_str()))
            .or_else(|| self.agent.first().map(|a| a.name.as_str()))
    }

    /// Resolve a source by name to either a provider or an agent.
    pub fn source_by_name(&self, name: &str) -> Option<SourceKind<'_>> {
        if let Some(p) = self.provider.iter().find(|p| p.name == name) {
            return Some(SourceKind::Provider(p));
        }
        if let Some(a) = self.agent.iter().find(|a| a.name == name) {
            return Some(SourceKind::Agent(a));
        }
        None
    }

    /// Active source from config defaults.
    pub fn active_source(&self) -> Option<SourceKind<'_>> {
        self.default_source_name()
            .and_then(|name| self.source_by_name(name))
    }

    /// Get the active provider config (default or first defined).
    /// Returns `None` when the active source is an ACP agent.
    pub fn active_provider(&self) -> Option<&ProviderConfig> {
        match self.active_source() {
            Some(SourceKind::Provider(p)) => Some(p),
            Some(SourceKind::Agent(_)) => None,
            None => self.provider.first(),
        }
    }

    /// Get the active agent config, if the default source is an agent.
    pub fn active_agent(&self) -> Option<&AgentConfig> {
        match self.active_source() {
            Some(SourceKind::Agent(a)) => Some(a),
            _ => None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_source: None,
            provider: vec![ProviderConfig {
                name: "gemini".to_string(),
                models: Some(vec!["gemini-2.5-flash".to_string()]),
                api_key_env: None,
                api_key_cmd: None,
                base_url: None,
                adapter: "gemini".to_string(),
                wire_protocol: None,
                temperature: None,
                top_p: None,
                max_tokens: None,
                additional_headers: HashMap::new(),
            }],
            agent: Vec::new(),
            tools: ToolsConfig::default(),
            mcp_servers: Vec::new(),
            default_persona: None,
        }
    }
}

/// Get the user's config directory via the `dirs` crate.
/// - Linux: $XDG_CONFIG_HOME or ~/.config
/// - macOS: ~/Library/Application Support
/// - Windows: {FOLDERID_RoamingAppData}
fn config_base_dir() -> Result<PathBuf> {
    dirs::config_dir().ok_or_else(|| eyre!("Could not determine config directory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_params_has_any_and_merged_with() {
        let base = ModelParams {
            temperature: Some(0.5),
            top_p: None,
            max_tokens: Some(100),
        };
        assert!(base.has_any());
        assert!(!ModelParams::default().has_any());

        let overrides = ModelParams {
            temperature: Some(0.9),
            top_p: Some(0.8),
            max_tokens: None,
        };
        let merged = base.merged_with(&overrides);
        assert_eq!(merged.temperature, Some(0.9));
        assert_eq!(merged.top_p, Some(0.8));
        assert_eq!(merged.max_tokens, Some(100));
    }

    #[test]
    fn toml_parse_and_active_provider() {
        let toml = r#"
default_source = "anthropic"

[[provider]]
name = "openai"
adapter = "openai"
models = ["gpt-4o-mini", "gpt-4o"]
temperature = 0.7

[[provider]]
name = "anthropic"
adapter = "anthropic"
wire_protocol = "auto"
models = ["claude-sonnet-4-0"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let active = config.active_provider().unwrap();
        assert_eq!(active.name, "anthropic");
        assert_eq!(active.wire_protocol, Some(WireProtocol::Auto));
        assert_eq!(active.default_model(), Some("claude-sonnet-4-0"));

        let openai = &config.provider[0];
        assert_eq!(openai.default_model(), Some("gpt-4o-mini"));
        assert_eq!(openai.model_params().temperature, Some(0.7));
    }

    #[test]
    fn active_provider_falls_back_to_first() {
        let config: Config = toml::from_str(
            r#"
[[provider]]
name = "openai"
adapter = "openai"
models = ["gpt-4o"]
"#,
        )
        .unwrap();
        assert_eq!(config.active_provider().unwrap().name, "openai");
    }

    #[test]
    fn wire_protocol_parses_and_defaults() {
        let config: Config = toml::from_str(
            r#"
[[provider]]
name = "opencode-go-responses"
adapter = "opencode_go"
wire_protocol = "responses"
models = ["muse-spark-1.3-contributor"]

[[provider]]
name = "opencode-go-auto"
adapter = "opencode_go"
models = ["muse-spark-1.2-contributor"]
"#,
        )
        .unwrap();

        assert_eq!(
            config.provider[0].wire_protocol,
            Some(WireProtocol::Responses)
        );
        assert_eq!(config.provider[1].wire_protocol, None);
    }

    #[test]
    fn validate_rejects_both_api_key_env_and_api_key_cmd() {
        let config: Config = toml::from_str(
            r#"
[[provider]]
name = "openai"
adapter = "openai"
models = ["gpt-4o"]
api_key_env = "OPENAI_API_KEY"
api_key_cmd = "echo secret"
"#,
        )
        .unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("openai"));
    }

    #[test]
    fn validate_allows_either_api_key_env_or_api_key_cmd_alone() {
        let config: Config = toml::from_str(
            r#"
[[provider]]
name = "openai"
adapter = "openai"
models = ["gpt-4o"]
api_key_env = "OPENAI_API_KEY"

[[provider]]
name = "anthropic"
adapter = "anthropic"
models = ["claude-sonnet-4-0"]
api_key_cmd = "echo secret"
"#,
        )
        .unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn agent_config_and_active_source() {
        let config: Config = toml::from_str(
            r#"
default_source = "opencode"

[[provider]]
name = "openai"
adapter = "openai"
models = ["gpt-4o"]

[[agent]]
name = "opencode"
command = "opencode"
args = ["acp"]
"#,
        )
        .unwrap();
        assert!(config.active_provider().is_none());
        let agent = config.active_agent().unwrap();
        assert_eq!(agent.name, "opencode");
        assert_eq!(agent.command, "opencode");
        assert_eq!(agent.args, vec!["acp"]);
    }
}
