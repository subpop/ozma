pub mod list_directory;
pub mod read_file;
pub mod run_command;
pub mod web_fetch;
pub mod web_search;
pub mod write_file;

use std::collections::HashMap;

use async_trait::async_trait;
use genai::chat::Tool as GenaiTool;
use genai::chat::ToolName;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::model::message::ContentPart;

/// Permission level required to execute a tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionLevel {
    /// Safe read-only operations — auto-approved by default.
    ReadOnly,
    /// Potentially destructive — requires user confirmation.
    Destructive,
    /// Executes arbitrary commands — always requires confirmation.
    Dangerous,
}

/// The result of executing a tool.
#[derive(Clone, Debug)]
pub enum ToolResult {
    /// Tool executed successfully — content parts shown to the LLM.
    Ok(Vec<ContentPart>),
    /// Tool execution failed — error message shown to the LLM.
    Err(String),
}

impl ToolResult {
    /// Whether the tool executed successfully.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }

    /// Convenience: create an Ok result from a plain text string.
    pub fn ok_text(text: impl Into<String>) -> Self {
        Self::Ok(vec![ContentPart::Text { text: text.into() }])
    }

    /// Consume into content parts. Errors are wrapped as a single text part.
    pub fn into_content_parts(self) -> Vec<ContentPart> {
        match self {
            Self::Ok(parts) => parts,
            Self::Err(e) => vec![ContentPart::Text { text: e }],
        }
    }
}

/// Trait for a built-in tool implementation.
#[async_trait]
pub trait ToolDef: Send + Sync {
    /// The unique name of this tool (used in the LLM function call).
    fn name(&self) -> &str;

    /// A human-readable description of what this tool does.
    fn description(&self) -> &str;

    /// The JSON Schema for the tool's parameters.
    fn parameters_schema(&self) -> Value;

    /// The permission level required to execute this tool.
    fn permission_level(&self) -> PermissionLevel;

    /// Execute the tool with the given arguments.
    /// The `cancel` token can be used to abort long-running operations on shutdown.
    async fn execute(&self, args: &Value, cancel: CancellationToken) -> ToolResult;

    /// A short summary of the tool call for display in the UI.
    /// Returns something like "read_file: src/main.rs" or "run_command: ls -la".
    fn display_summary(&self, args: &Value) -> String {
        format!("{}: {}", self.name(), args)
    }
}

/// Registry holding all available tools. Provides lookup by name and
/// generates the genai `Tool` definitions for the ChatRequest.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn ToolDef>>,
}

impl ToolRegistry {
    /// Create a new registry with all built-in tools.
    pub fn new() -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };

        registry.register(Box::new(read_file::ReadFile));
        registry.register(Box::new(write_file::WriteFile));
        registry.register(Box::new(list_directory::ListDirectory));
        registry.register(Box::new(run_command::RunCommand));
        registry.register(Box::new(web_fetch::WebFetch));
        registry.register(Box::new(web_search::WebSearch));

        registry
    }

    fn register(&mut self, tool: Box<dyn ToolDef>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Get a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn ToolDef> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Generate genai `Tool` definitions for all registered tools.
    pub fn genai_tools(&self) -> Vec<GenaiTool> {
        self.tools
            .values()
            .map(|tool| {
                GenaiTool::new(ToolName::Custom(tool.name().to_string()))
                    .with_description(tool.description())
                    .with_schema(tool.parameters_schema())
            })
            .collect()
    }

    /// Get a display summary for a tool call.
    pub fn display_summary(&self, name: &str, args: &Value) -> String {
        if let Some(tool) = self.get(name) {
            tool.display_summary(args)
        } else {
            // For MCP tools (namespaced as server__tool), show a cleaner format
            if let Some((_server, tool_name)) = name.split_once("__") {
                format!("{}: {}", tool_name, args)
            } else {
                format!("{}: {}", name, args)
            }
        }
    }

    /// Check whether a tool name is a built-in tool.
    pub fn is_builtin(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_ok_and_err() {
        let ok = ToolResult::ok_text("hello");
        assert!(ok.is_ok());
        let parts = ToolResult::ok_text("hello").into_content_parts();
        assert!(matches!(&parts[0], ContentPart::Text { text } if text == "hello"));

        let err = ToolResult::Err("boom".into());
        assert!(!err.is_ok());
        let parts = err.into_content_parts();
        assert!(matches!(&parts[0], ContentPart::Text { text } if text == "boom"));
    }

    #[test]
    fn registry_get_and_is_builtin() {
        let registry = ToolRegistry::new();
        assert!(registry.get("read_file").is_some());
        assert!(registry.is_builtin("read_file"));
        assert!(!registry.is_builtin("server__mcp_tool"));
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn display_summary_for_mcp_style_name() {
        let registry = ToolRegistry::new();
        let args = serde_json::json!({"x": 1});
        let summary = registry.display_summary("github__search", &args);
        assert!(summary.starts_with("search:"));
    }
}
