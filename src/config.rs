use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Top-level application configuration, loaded from `~/.config/ozma/config.json`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    /// Default source name (provider or agent).
    #[serde(default)]
    pub default_source: Option<String>,

    /// Provider configurations keyed by provider name.
    #[serde(default)]
    pub provider: BTreeMap<String, ProviderConfig>,

    /// ACP agent configurations keyed by agent name.
    #[serde(default)]
    pub agent: BTreeMap<String, AgentConfig>,

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
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentConfig {
    /// Agent name. In config JSON this is inferred from the `agent` map key.
    #[serde(default)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// Provider name. In config JSON this is inferred from the `provider` map key.
    #[serde(default)]
    pub name: String,

    /// Available model identifiers (e.g., ["gpt-4o-mini", "gpt-4o", "o3-mini"]).
    /// When omitted, ozma queries the provider at startup to discover available models.
    #[serde(default)]
    pub models: Option<Vec<String>>,

    /// Explicit default model for this provider.
    /// If unset, ozma falls back to the first configured/discovered model.
    #[serde(default)]
    pub default_model: Option<String>,

    /// Environment variable name holding the API key (e.g., "OPENAI_API_KEY").
    /// If not set, genai's default env var resolution is used.
    #[serde(default)]
    pub api_key_env: Option<String>,

    /// Shell command to run to retrieve the API key (e.g. a password manager CLI).
    /// The command is run via `sh -c` (`cmd /C` on Windows) on every request, and its
    /// trimmed stdout is used as the key. Mutually exclusive with `api_key_env`.
    #[serde(default)]
    pub api_key_cmd: Option<String>,

    /// Optional custom base URL for the API endpoint (e.g. "http://localhost:11434/v1/").
    /// Overrides the adapter's default endpoint when set.
    #[serde(default)]
    pub base_url: Option<String>,

    /// The protocol adapter to use (e.g. "openai", "anthropic", "gemini", "ollama",
    /// "opencode_go"). Acts as a shorthand for the provider's base values: the
    /// default wire format, endpoint, and API key env var. Explicit `wire_protocol`,
    /// `base_url`, and `api_key_env`/`api_key_cmd` values take precedence over
    /// the adapter defaults.
    ///
    /// When omitted, the provider must be fully defined: `base_url`,
    /// `api_key_env` or `api_key_cmd`, and a concrete `wire_protocol`
    /// (`chat_completions`, `responses`, or `messages` — not `auto`).
    #[serde(default)]
    pub adapter: Option<String>,

    /// Optional wire protocol override for adapters that expose multiple protocols
    /// behind a single endpoint (e.g. OpenCode Go).
    ///
    /// Supported values:
    /// - "auto" (default behavior)
    /// - "chat_completions"
    /// - "responses"
    /// - "messages"
    ///
    /// Required when `adapter` is omitted (and must not be "auto", since there
    /// is no adapter-native protocol to fall back to).
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
    /// The default model for this provider.
    /// Prefers explicit `default_model`, then falls back to the first configured model.
    pub fn default_model(&self) -> Option<&str> {
        self.default_model.as_deref().or_else(|| {
            self.models
                .as_ref()
                .and_then(|models| models.first().map(String::as_str))
        })
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
    /// Load configuration from `~/.config/ozma/config.json` with optional
    /// per-directory overrides from `.ozma.json` in the current working directory.
    ///
    /// `.ozma.json` is layered on top of the user config and overrides matching keys.
    /// If the user config file doesn't exist, defaults are used as the base layer.
    pub fn load() -> Result<Self> {
        let user_path = Self::config_path()?;
        let local_path = Self::local_config_path();

        let mut config = if user_path.exists() {
            let mut cfg = Self::parse_config_file(&user_path)?;
            cfg.normalize_names();
            cfg
        } else {
            info!("No config file at {:?}, using defaults", user_path);
            Self::default()
        };

        if local_path.exists() {
            let local_overrides = Self::parse_value_file(&local_path)?;
            let mut merged = serde_json::to_value(&config).map_err(|e| {
                eyre!(
                    "Failed to serialize base config for layering {:?}: {}",
                    user_path,
                    e
                )
            })?;
            merge_json_values(&mut merged, local_overrides);
            config = serde_json::from_value(merged).map_err(|e| {
                eyre!(
                    "Failed to parse layered config with local overrides {:?}: {}",
                    local_path,
                    e
                )
            })?;
            config.normalize_names();
            info!("Applied local config overrides from {:?}", local_path);
        }

        config.validate()?;

        info!(
            "Loaded config ({} providers) from user {:?}{}",
            config.provider.len(),
            user_path,
            if local_path.exists() {
                format!(", local {:?}", local_path)
            } else {
                String::new()
            }
        );

        Ok(config)
    }

    /// Validate cross-field constraints that serde can't express on its own.
    fn validate(&self) -> Result<()> {
        for (provider_name, provider) in &self.provider {
            if provider.api_key_env.is_some() && provider.api_key_cmd.is_some() {
                return Err(eyre!(
                    "Provider '{}' sets both api_key_env and api_key_cmd; only one may be set",
                    provider_name
                ));
            }

            // Without an adapter there are no base-value defaults, so the
            // provider must be fully defined.
            if provider.adapter.is_none() {
                let mut missing: Vec<&str> = Vec::new();
                if provider.base_url.is_none() {
                    missing.push("base_url");
                }
                if provider.api_key_env.is_none() && provider.api_key_cmd.is_none() {
                    missing.push("api_key_env or api_key_cmd");
                }
                match provider.wire_protocol {
                    None | Some(WireProtocol::Auto) => missing.push("wire_protocol"),
                    Some(_) => {}
                }
                if !missing.is_empty() {
                    return Err(eyre!(
                        "Provider '{}' has no adapter and is missing: {} (set an adapter, or define the provider fully with base_url, an api key, and a concrete wire_protocol)",
                        provider_name,
                        missing.join(", ")
                    ));
                }
            }
        }
        Ok(())
    }

    fn normalize_names(&mut self) {
        for (name, provider) in &mut self.provider {
            provider.name = name.clone();
        }
        for (name, agent) in &mut self.agent {
            agent.name = name.clone();
        }
    }

    /// Get the path to the user-level config file.
    fn config_path() -> Result<PathBuf> {
        let config_dir = config_base_dir()?;
        Ok(config_dir.join("ozma").join("config.json"))
    }

    /// Get the path to the local per-directory config override file.
    fn local_config_path() -> PathBuf {
        PathBuf::from(".ozma.json")
    }

    fn parse_config_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| eyre!("Failed to read config file {:?}: {}", path, e))?;

        serde_json::from_str(&contents)
            .map_err(|e| eyre!("Failed to parse config file {:?}: {}", path, e))
    }

    fn parse_value_file(path: &Path) -> Result<serde_json::Value> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| eyre!("Failed to read config file {:?}: {}", path, e))?;

        serde_json::from_str(&contents)
            .map_err(|e| eyre!("Failed to parse config file {:?}: {}", path, e))
    }

    /// Default source name: `default_source`, else first provider/agent.
    pub fn default_source_name(&self) -> Option<&str> {
        self.default_source
            .as_deref()
            .or_else(|| self.provider.keys().next().map(|name| name.as_str()))
            .or_else(|| self.agent.keys().next().map(|name| name.as_str()))
    }

    /// Resolve a source by name to either a provider or an agent.
    pub fn source_by_name(&self, name: &str) -> Option<SourceKind<'_>> {
        if let Some(p) = self.provider.get(name) {
            return Some(SourceKind::Provider(p));
        }
        if let Some(a) = self.agent.get(name) {
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
            None => self.provider.values().next(),
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
        let mut provider = BTreeMap::new();
        provider.insert(
            "gemini".to_string(),
            ProviderConfig {
                name: "gemini".to_string(),
                models: Some(vec!["gemini-2.5-flash".to_string()]),
                default_model: None,
                api_key_env: None,
                api_key_cmd: None,
                base_url: None,
                adapter: Some("gemini".to_string()),
                wire_protocol: None,
                temperature: None,
                top_p: None,
                max_tokens: None,
                additional_headers: HashMap::new(),
            },
        );

        Self {
            default_source: None,
            provider,
            agent: BTreeMap::new(),
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

fn merge_json_values(base: &mut serde_json::Value, overlay: serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base_obj), serde_json::Value::Object(overlay_obj)) => {
            for (key, overlay_value) in overlay_obj {
                if let Some(base_value) = base_obj.get_mut(&key) {
                    merge_json_values(base_value, overlay_value);
                } else {
                    base_obj.insert(key, overlay_value);
                }
            }
        }
        (serde_json::Value::Array(base_array), serde_json::Value::Array(overlay_array)) => {
            base_array.extend(overlay_array);
        }
        (base_value, overlay_value) => {
            *base_value = overlay_value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_config(json: &str) -> Config {
        let mut config: Config = serde_json::from_str(json).unwrap();
        config.normalize_names();
        config
    }

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
    fn json_parse_and_active_provider() {
        let config = parse_config(
            r#"{
  "default_source": "anthropic",
  "provider": {
    "openai": {
      "adapter": "openai",
      "models": ["gpt-4o-mini", "gpt-4o"],
      "temperature": 0.7
    },
    "anthropic": {
      "adapter": "anthropic",
      "wire_protocol": "auto",
      "models": ["claude-sonnet-4-0"]
    }
  }
}"#,
        );
        let active = config.active_provider().unwrap();
        assert_eq!(active.name, "anthropic");
        assert_eq!(active.wire_protocol, Some(WireProtocol::Auto));
        assert_eq!(active.default_model(), Some("claude-sonnet-4-0"));

        let openai = config.provider.get("openai").unwrap();
        assert_eq!(openai.default_model(), Some("gpt-4o-mini"));
        assert_eq!(openai.model_params().temperature, Some(0.7));
    }

    #[test]
    fn provider_default_model_prefers_explicit_field() {
        let config = parse_config(
            r#"{
  "provider": {
    "openai": {
      "adapter": "openai",
      "models": ["gpt-4o-mini", "gpt-4o"],
      "default_model": "gpt-4o"
    },
    "anthropic": {
      "adapter": "anthropic",
      "default_model": "claude-sonnet-4-0"
    }
  }
}"#,
        );

        assert_eq!(config.provider["openai"].default_model(), Some("gpt-4o"));
        assert_eq!(
            config.provider["anthropic"].default_model(),
            Some("claude-sonnet-4-0")
        );
    }

    #[test]
    fn active_provider_falls_back_to_first() {
        let config = parse_config(
            r#"{
  "provider": {
    "openai": {
      "adapter": "openai",
      "models": ["gpt-4o"]
    }
  }
}"#,
        );
        assert_eq!(config.active_provider().unwrap().name, "openai");
    }

    #[test]
    fn wire_protocol_parses_and_defaults() {
        let config = parse_config(
            r#"{
  "provider": {
    "opencode-go-responses": {
      "adapter": "opencode_go",
      "wire_protocol": "responses",
      "models": ["muse-spark-1.3-contributor"]
    },
    "opencode-go-auto": {
      "adapter": "opencode_go",
      "models": ["muse-spark-1.2-contributor"]
    }
  }
}"#,
        );

        assert_eq!(
            config.provider["opencode-go-responses"].wire_protocol,
            Some(WireProtocol::Responses)
        );
        assert_eq!(config.provider["opencode-go-auto"].wire_protocol, None);
    }

    #[test]
    fn validate_rejects_both_api_key_env_and_api_key_cmd() {
        let config = parse_config(
            r#"{
  "provider": {
    "openai": {
      "adapter": "openai",
      "models": ["gpt-4o"],
      "api_key_env": "OPENAI_API_KEY",
      "api_key_cmd": "echo secret"
    }
  }
}"#,
        );
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("openai"));
    }

    #[test]
    fn validate_allows_either_api_key_env_or_api_key_cmd_alone() {
        let config = parse_config(
            r#"{
  "provider": {
    "openai": {
      "adapter": "openai",
      "models": ["gpt-4o"],
      "api_key_env": "OPENAI_API_KEY"
    },
    "anthropic": {
      "adapter": "anthropic",
      "models": ["claude-sonnet-4-0"],
      "api_key_cmd": "echo secret"
    }
  }
}"#,
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn adapter_less_provider_parses_and_validates() {
        let config = parse_config(
            r#"{
  "provider": {
    "my-gateway": {
      "base_url": "https://gateway.example.com/v1/",
      "api_key_env": "GATEWAY_API_KEY",
      "wire_protocol": "chat_completions",
      "models": ["some-model"]
    }
  }
}"#,
        );
        assert!(config.validate().is_ok());
        let provider = config.provider.get("my-gateway").unwrap();
        assert_eq!(provider.adapter, None);
        assert_eq!(provider.wire_protocol, Some(WireProtocol::ChatCompletions));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://gateway.example.com/v1/")
        );
    }

    #[test]
    fn validate_rejects_adapter_less_provider_missing_fields() {
        // Missing everything.
        let config = parse_config(
            r#"{
  "provider": {
    "incomplete": {}
  }
}"#,
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("incomplete"));
        assert!(err.contains("missing: base_url, api_key_env or api_key_cmd, wire_protocol"));

        // base_url and key set, but wire_protocol missing.
        let config = parse_config(
            r#"{
  "provider": {
    "incomplete": {
      "base_url": "https://gateway.example.com/v1/",
      "api_key_env": "GATEWAY_API_KEY"
    }
  }
}"#,
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("missing: wire_protocol"));
        assert!(!err.contains("missing: base_url"));

        // wire_protocol set but no base_url.
        let config = parse_config(
            r#"{
  "provider": {
    "incomplete": {
      "api_key_cmd": "echo secret",
      "wire_protocol": "responses"
    }
  }
}"#,
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("missing: base_url"));

        // No api key.
        let config = parse_config(
            r#"{
  "provider": {
    "incomplete": {
      "base_url": "https://gateway.example.com/v1/",
      "wire_protocol": "messages"
    }
  }
}"#,
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("missing: api_key_env or api_key_cmd"));
    }

    #[test]
    fn validate_rejects_adapter_less_provider_with_auto_wire_protocol() {
        let config = parse_config(
            r#"{
  "provider": {
    "auto-no-adapter": {
      "base_url": "https://gateway.example.com/v1/",
      "api_key_env": "GATEWAY_API_KEY",
      "wire_protocol": "auto"
    }
  }
}"#,
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("auto-no-adapter"));
        assert!(err.contains("wire_protocol"));
    }

    #[test]
    fn agent_config_and_active_source() {
        let config = parse_config(
            r#"{
  "default_source": "opencode",
  "provider": {
    "openai": {
      "adapter": "openai",
      "models": ["gpt-4o"]
    }
  },
  "agent": {
    "opencode": {
      "command": "opencode",
      "args": ["acp"]
    }
  }
}"#,
        );
        assert!(config.active_provider().is_none());
        let agent = config.active_agent().unwrap();
        assert_eq!(agent.name, "opencode");
        assert_eq!(agent.command, "opencode");
        assert_eq!(agent.args, vec!["acp"]);
    }

    #[test]
    fn layered_override_merges_nested_objects() {
        let mut base = serde_json::to_value(&Config {
            default_source: Some("openai".to_string()),
            provider: BTreeMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    name: "openai".to_string(),
                    models: Some(vec!["gpt-4o".to_string()]),
                    default_model: None,
                    api_key_env: None,
                    api_key_cmd: None,
                    base_url: None,
                    adapter: Some("openai".to_string()),
                    wire_protocol: None,
                    temperature: None,
                    top_p: None,
                    max_tokens: None,
                    additional_headers: HashMap::new(),
                },
            )]),
            agent: BTreeMap::new(),
            tools: ToolsConfig {
                auto_approve_read: true,
                auto_approve_write: false,
                auto_approve_execute: false,
            },
            mcp_servers: Vec::new(),
            default_persona: None,
        })
        .unwrap();

        let local: serde_json::Value = serde_json::from_str(
            r#"{
  "default_source": "anthropic",
  "tools": {
    "auto_approve_write": true
  }
}"#,
        )
        .unwrap();

        merge_json_values(&mut base, local);
        let mut merged: Config = serde_json::from_value(base).unwrap();
        merged.normalize_names();

        assert_eq!(merged.default_source.as_deref(), Some("anthropic"));
        assert!(merged.tools.auto_approve_read);
        assert!(merged.tools.auto_approve_write);
        assert!(!merged.tools.auto_approve_execute);
    }

    #[test]
    fn layered_override_merges_provider_map_by_name() {
        let mut base = serde_json::to_value(&Config::default()).unwrap();

        let local: serde_json::Value = serde_json::from_str(
            r#"{
  "provider": {
    "gemini": {
      "models": ["gemini-2.5-pro"]
    },
    "openai": {
      "adapter": "openai",
      "models": ["gpt-4o-mini"]
    }
  }
}"#,
        )
        .unwrap();

        merge_json_values(&mut base, local);
        let mut merged: Config = serde_json::from_value(base).unwrap();
        merged.normalize_names();

        assert_eq!(merged.provider.len(), 2);
        assert_eq!(
            merged.provider["gemini"].models,
            Some(vec![
                "gemini-2.5-flash".to_string(),
                "gemini-2.5-pro".to_string()
            ])
        );
        assert_eq!(
            merged.provider["gemini"].default_model(),
            Some("gemini-2.5-flash")
        );
        assert_eq!(merged.provider["openai"].name, "openai");
        assert_eq!(
            merged.provider["openai"].default_model(),
            Some("gpt-4o-mini")
        );
    }
}
