use std::fmt;

use chrono::{DateTime, Utc};
use genai::chat::Usage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Rough token estimation: ~4 characters per token for English text.
/// This is a widely-used heuristic that's reasonably accurate across models.
const CHARS_PER_TOKEN: usize = 4;

/// Flat estimate for an image's token cost (low-detail mode).
const IMAGE_TOKEN_ESTIMATE: u32 = 300;

/// The role of a message sender.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Role {
    /// System prompt / instructions
    System,
    /// The human user
    User,
    /// The LLM assistant
    Assistant,
    /// Tool result
    Tool,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Role::System => write!(f, "system"),
            Role::User => write!(f, "you"),
            Role::Assistant => write!(f, "assistant"),
            Role::Tool => write!(f, "tool"),
        }
    }
}

/// A single part of a message's content.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    /// Plain text content.
    #[serde(rename = "text")]
    Text { text: String },
    /// An image attachment (data lives in the `attachments` table).
    #[serde(rename = "image")]
    Image {
        id: Uuid,
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
    },
}

/// A tool call made by the assistant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCallInfo {
    /// The call ID from the provider (used to correlate with ToolResponse).
    pub call_id: String,
    /// The function name.
    pub fn_name: String,
    /// The function arguments as JSON.
    pub fn_arguments: Value,
}

/// A tool execution result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultInfo {
    /// The call ID this result corresponds to.
    pub call_id: String,
    /// The function name (needed for some providers like Gemini).
    pub fn_name: String,
    /// Whether the tool execution succeeded.
    pub success: bool,
}

/// Actual token usage reported by the provider for a single API call.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub reasoning_tokens: Option<i32>,
}

impl TokenUsage {
    /// Accumulate another usage report into this one.
    pub fn add(&mut self, other: &TokenUsage) {
        fn add_opt(a: &mut Option<i32>, b: Option<i32>) {
            match (a.as_mut(), b) {
                (Some(a), Some(b)) => *a += b,
                (None, Some(b)) => *a = Some(b),
                _ => {}
            }
        }
        add_opt(&mut self.prompt_tokens, other.prompt_tokens);
        add_opt(&mut self.completion_tokens, other.completion_tokens);
        add_opt(&mut self.total_tokens, other.total_tokens);
        add_opt(&mut self.cached_tokens, other.cached_tokens);
        add_opt(&mut self.reasoning_tokens, other.reasoning_tokens);
    }
}

impl From<Usage> for TokenUsage {
    fn from(u: Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cached_tokens: u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            reasoning_tokens: u
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens),
        }
    }
}

/// A single message in a conversation.
#[derive(Clone, Debug)]
pub struct Message {
    /// Unique identifier.
    pub id: Uuid,
    /// Who sent this message.
    pub role: Role,
    /// The content parts of the message (text, images, etc.).
    pub content: Vec<ContentPart>,
    /// When this message was created.
    pub timestamp: DateTime<Utc>,
    /// Whether this message is still being streamed (assistant only).
    pub streaming: bool,
    /// Whether this message has been compacted (summarized and excluded from LLM context).
    /// Compacted messages are kept for playback but replaced by a summary in the active context.
    pub compacted: bool,
    /// Tool calls made by the assistant (only set on assistant messages).
    pub tool_calls: Vec<ToolCallInfo>,
    /// Tool result info (only set on tool messages).
    pub tool_result: Option<ToolResultInfo>,
    /// Actual token usage reported by the provider (set on assistant messages after completion).
    pub usage: Option<TokenUsage>,
}

impl Message {
    /// Create a new user message with text content.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role: Role::User,
            content: vec![ContentPart::Text {
                text: content.into(),
            }],
            timestamp: Utc::now(),
            streaming: false,
            compacted: false,
            tool_calls: Vec::new(),
            tool_result: None,
            usage: None,
        }
    }

    /// Create a new assistant message (initially empty, for streaming).
    pub fn assistant_streaming() -> Self {
        Self {
            id: Uuid::new_v4(),
            role: Role::Assistant,
            content: vec![ContentPart::Text {
                text: String::new(),
            }],
            timestamp: Utc::now(),
            streaming: true,
            compacted: false,
            tool_calls: Vec::new(),
            tool_result: None,
            usage: None,
        }
    }

    /// Create a system message (used for compaction summaries).
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role: Role::System,
            content: vec![ContentPart::Text {
                text: content.into(),
            }],
            timestamp: Utc::now(),
            streaming: false,
            compacted: false,
            tool_calls: Vec::new(),
            tool_result: None,
            usage: None,
        }
    }

    /// Create a tool result message.
    pub fn tool_result(
        call_id: String,
        fn_name: String,
        content: Vec<ContentPart>,
        success: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            role: Role::Tool,
            content,
            timestamp: Utc::now(),
            streaming: false,
            compacted: false,
            tool_calls: Vec::new(),
            tool_result: Some(ToolResultInfo {
                call_id,
                fn_name,
                success,
            }),
            usage: None,
        }
    }

    /// Append a chunk of text to this message (used during streaming).
    /// Appends to the last Text part, or creates one if none exists.
    pub fn append(&mut self, chunk: &str) {
        if let Some(ContentPart::Text { text }) = self.content.last_mut() {
            text.push_str(chunk);
        } else {
            self.content.push(ContentPart::Text {
                text: chunk.to_string(),
            });
        }
    }

    /// Mark the message as complete (no longer streaming).
    pub fn complete(&mut self) {
        self.streaming = false;
    }

    /// Add a tool call to this assistant message.
    #[allow(dead_code)]
    pub fn add_tool_call(&mut self, call_id: String, fn_name: String, fn_arguments: Value) {
        self.tool_calls.push(ToolCallInfo {
            call_id,
            fn_name,
            fn_arguments,
        });
    }

    /// Whether this message contains tool calls.
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// Join all text parts into a single string (for display, title derivation, etc.).
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Whether this message contains any image parts.
    pub fn has_images(&self) -> bool {
        self.content
            .iter()
            .any(|p| matches!(p, ContentPart::Image { .. }))
    }

    /// Add an image attachment to this message.
    pub fn add_image(&mut self, id: Uuid, mime_type: String, alt: Option<String>) {
        self.content.push(ContentPart::Image { id, mime_type, alt });
    }

    /// Estimate the token count for this message's content.
    pub fn estimate_tokens(&self) -> u32 {
        let content_tokens: u32 = self
            .content
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => {
                    ((text.len() + CHARS_PER_TOKEN - 1) / CHARS_PER_TOKEN) as u32
                }
                ContentPart::Image { .. } => IMAGE_TOKEN_ESTIMATE,
            })
            .sum();
        content_tokens + 4 // ~4 tokens overhead per message for role/framing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_add_sums_and_fills_none() {
        let mut a = TokenUsage {
            prompt_tokens: Some(10),
            completion_tokens: None,
            total_tokens: Some(10),
            cached_tokens: None,
            reasoning_tokens: Some(1),
        };
        let b = TokenUsage {
            prompt_tokens: Some(5),
            completion_tokens: Some(3),
            total_tokens: Some(8),
            cached_tokens: Some(2),
            reasoning_tokens: None,
        };
        a.add(&b);
        assert_eq!(a.prompt_tokens, Some(15));
        assert_eq!(a.completion_tokens, Some(3));
        assert_eq!(a.total_tokens, Some(18));
        assert_eq!(a.cached_tokens, Some(2));
        assert_eq!(a.reasoning_tokens, Some(1));
    }

    #[test]
    fn text_content_joins_text_skips_images() {
        let mut msg = Message::user("hello");
        msg.add_image(Uuid::nil(), "image/png".to_string(), None);
        msg.append(" world");
        assert_eq!(msg.text_content(), "hello world");
        assert!(msg.has_images());
        assert!(!msg.has_tool_calls());
    }

    #[test]
    fn append_complete_and_tool_calls() {
        let mut msg = Message::assistant_streaming();
        assert!(msg.streaming);
        msg.append("Hi");
        msg.complete();
        assert!(!msg.streaming);
        assert_eq!(msg.text_content(), "Hi");
        msg.add_tool_call("1".into(), "read_file".into(), serde_json::json!({}));
        assert!(msg.has_tool_calls());
    }

    #[test]
    fn estimate_tokens_text_and_image() {
        let msg = Message::user("abcd"); // 4 chars → 1 token + 4 overhead
        assert_eq!(msg.estimate_tokens(), 5);
        let mut with_img = Message::user("");
        with_img.content.clear();
        with_img.add_image(Uuid::nil(), "image/png".to_string(), None);
        assert_eq!(with_img.estimate_tokens(), IMAGE_TOKEN_ESTIMATE + 4);
    }
}
