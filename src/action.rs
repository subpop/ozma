use serde_json::Value;
use uuid::Uuid;

use crate::acp::{SessionRef, SessionSummary};
use crate::components::input::PendingAttachment;
use crate::model::conversation::Conversation;
use crate::model::message::{ContentPart, TokenUsage, ToolCallInfo};
use crate::pricing::PricingCatalog;
use crate::tools::PermissionLevel;

/// Actions represent application-level intents that flow through the update cycle.
/// Components translate Events into Actions, and the App dispatches Actions back to components.
#[derive(Clone, Debug)]
pub enum Action {
    /// No-op, do nothing
    Noop,
    /// Periodic tick for background logic
    Tick,
    /// Render a frame
    Render,
    /// Quit the application
    Quit,
    /// Terminal was resized
    #[allow(dead_code)]
    Resize(u16, u16),

    // -- Chat actions --
    /// User submitted a message from the input, with optional image attachments.
    SubmitMessage {
        text: String,
        attachments: Vec<PendingAttachment>,
    },
    /// A chunk of text arrived from the assistant (streaming)
    AppendAssistantChunk(String),
    /// The assistant finished responding
    AssistantMessageComplete { usage: Option<TokenUsage> },
    /// An error occurred during streaming
    StreamError(String),
    /// Text paste from bracketed paste event.
    PasteText(String),
    /// Trigger sending the conversation to the LLM provider
    SendMessage,
    /// Scroll the conversation view up (3 lines)
    ScrollUp,
    /// Scroll the conversation view down (3 lines)
    ScrollDown,
    /// Scroll the conversation view up by a full page
    ScrollPageUp,
    /// Scroll the conversation view down by a full page
    ScrollPageDown,
    /// Scroll to the bottom of the conversation
    #[allow(dead_code)]
    ScrollToBottom,
    /// Focus the input area
    FocusInput,
    /// Internal: carries copied content to the clipboard handler
    CopyCodeBlockResult(String),
    /// A mouse click at (column, row) in the conversation area — used for block selection.
    ClickAt(u16, u16),
    /// Copy the currently selected block (message or code block) to clipboard.
    CopySelection,

    // -- Tool calling actions --
    /// The assistant requested tool calls (streaming complete, tool calls accumulated).
    ToolCallsReceived {
        tool_calls: Vec<ToolCallInfo>,
        usage: Option<TokenUsage>,
    },
    /// Request permission to execute a tool.
    /// (tool_name, call_id, arguments, permission_level)
    ToolPermissionRequest {
        call_id: String,
        tool_name: String,
        arguments: Value,
        permission_level: PermissionLevel,
    },
    /// User approved a tool execution.
    ToolPermissionGranted { call_id: String },
    /// User denied a tool execution.
    ToolPermissionDenied { call_id: String },
    /// Approve all pending tool permissions.
    ToolPermissionGrantAll,
    /// A tool finished executing. (call_id, fn_name, content parts, success, blobs)
    ToolResultReady {
        call_id: String,
        fn_name: String,
        content: Vec<ContentPart>,
        success: bool,
        /// Binary blobs from MCP image responses to persist as attachments.
        blobs: Vec<(Uuid, String, Vec<u8>)>,
    },
    /// All tool results are in — send them back to the LLM for continuation.
    ToolResultsComplete,

    // -- Session management actions --
    /// Start a new conversation session
    NewSession,
    /// Open the session list overlay
    OpenSessionList,
    /// Close the session list overlay
    CloseSessionList,
    /// Load a conversation by ID
    LoadSession(SessionRef),
    /// A conversation was loaded from the database
    SessionLoaded(Box<Conversation>),
    /// Delete a conversation by ID
    DeleteSession(SessionRef),
    /// Request the session list to be refreshed from the database
    RefreshSessionList,
    /// The session list was loaded from the database
    SessionsLoaded(Vec<SessionSummary>),

    // -- Command palette actions --
    /// Open the command palette overlay
    OpenCommandPalette,
    /// Close the command palette overlay
    CloseCommandPalette,

    // -- Model switching --
    /// Switch to a different model. Contains (model_name, provider_name).
    SwitchModel(String, String),
    /// Open the model picker overlay
    OpenModelPicker,
    /// Close the model picker overlay
    CloseModelPicker,
    /// The model picker expanded a provider node whose models aren't known yet;
    /// fetch them from the provider's API. Contains the provider name.
    RequestProviderModels(String),
    /// A provider's model list finished loading. Contains (provider_name, models).
    ProviderModelsFetched(String, Vec<String>),

    // -- Persona switching --
    /// Switch to a different persona by name.
    SwitchPersona(String),
    /// Open the persona picker overlay.
    OpenPersonaPicker,
    /// Close the persona picker overlay.
    ClosePersonaPicker,

    // -- Cancellation --
    /// Cancel the current operation (streaming, tool execution, compaction)
    CancelOperation,

    // -- Context compaction actions --
    /// Trigger context compaction (manual or auto)
    CompactContext,
    /// Compaction completed with summary text and IDs of compacted messages
    CompactionComplete {
        summary: String,
        compacted_ids: Vec<Uuid>,
        usage: Option<TokenUsage>,
    },
    /// Compaction failed with error message
    CompactionError(String),

    // -- Title generation actions --
    /// LLM-generated title arrived for the conversation
    TitleGenerated {
        title: String,
        usage: Option<TokenUsage>,
    },

    // -- Export actions --
    /// Export the current conversation to a Markdown file
    ExportMarkdown,
    /// Export the current conversation to a JSON file
    ExportJson,
    /// Copy the current conversation as Markdown to the clipboard
    ExportMarkdownClipboard,
    /// Copy the current conversation as JSON to the clipboard
    ExportJsonClipboard,
    /// Export completed successfully (message to display)
    ExportComplete(String),
    /// Export failed (error message to display)
    ExportError(String),

    // -- Model parameter actions --
    /// Open the parameter input overlay for a specific parameter
    OpenParamInput(ParamKind),
    /// Close the parameter input overlay without changes
    CloseParamInput,
    /// Set the temperature override for the current conversation
    SetTemperature(Option<f64>),
    /// Set the top_p override for the current conversation
    SetTopP(Option<f64>),
    /// Set the max_tokens override for the current conversation
    SetMaxTokens(Option<u32>),

    // -- Pricing actions --
    /// Pricing catalog loaded from models.dev
    PricingLoaded(PricingCatalog),

    // -- ACP display / replay --
    /// Show an ACP tool call in the conversation.
    AcpToolCallDisplay(ToolCallInfo),
    /// Show an ACP tool-call update in the conversation.
    AcpToolCallUpdateDisplay(crate::acp::ToolCallUpdateSummary),
    /// Replay a user message chunk from ACP session/load.
    AcpUserMessageReplay(String),
    /// ACP usage/context update.
    AcpUsageUpdate(TokenUsage),
    /// ACP session/load finished.
    AcpSessionLoadComplete,
}

/// Which model parameter to edit.
#[derive(Clone, Debug)]
pub enum ParamKind {
    Temperature,
    TopP,
    MaxTokens,
}
