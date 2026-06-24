//! Map ACP types onto ozma domain types / events.

use agent_client_protocol::schema::v1::{
    ContentBlock, EmbeddedResourceResource, SessionUpdate, TextContent, ToolCall, ToolCallContent,
    ToolCallUpdate,
};
use serde_json::Value;

use crate::event::Event;
use crate::model::message::{TokenUsage, ToolCallInfo};

/// Convert an ACP content block into plain text for the chat UI.
pub fn content_block_text(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(TextContent { text, .. }) => Some(text.clone()),
        ContentBlock::Image(img) => Some(format!("[Image: {}]", img.mime_type)),
        ContentBlock::Audio(audio) => Some(format!("[Audio: {}]", audio.mime_type)),
        ContentBlock::ResourceLink(link) => Some(link.uri.clone()),
        ContentBlock::Resource(resource) => match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(text) => Some(text.text.clone()),
            EmbeddedResourceResource::BlobResourceContents(blob) => {
                Some(format!("[Blob: {}]", blob.uri))
            }
            _ => None,
        },
        _ => None,
    }
}

fn tool_call_content_text(content: &ToolCallContent) -> Option<String> {
    match content {
        ToolCallContent::Content(c) => content_block_text(&c.content),
        ToolCallContent::Diff(diff) => {
            let path = diff.path.display();
            Some(format!("[Diff: {path}]"))
        }
        ToolCallContent::Terminal(term) => Some(format!("[Terminal: {}]", term.terminal_id.0)),
        _ => None,
    }
}

/// Map a session update notification into zero or more TUI events.
pub fn session_update_to_events(update: SessionUpdate) -> Vec<Event> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => content_block_text(&chunk.content)
            .map(Event::StreamChunk)
            .into_iter()
            .collect(),
        SessionUpdate::AgentThoughtChunk(chunk) => content_block_text(&chunk.content)
            .map(|t| Event::StreamChunk(format!("*{t}*")))
            .into_iter()
            .collect(),
        SessionUpdate::UserMessageChunk(chunk) => content_block_text(&chunk.content)
            .map(Event::AcpUserMessageChunk)
            .into_iter()
            .collect(),
        SessionUpdate::ToolCall(tool_call) => {
            vec![Event::AcpToolCall(tool_call_info_from_acp(&tool_call))]
        }
        SessionUpdate::ToolCallUpdate(update) => {
            vec![Event::AcpToolCallUpdate(tool_call_update_summary(update))]
        }
        SessionUpdate::SessionInfoUpdate(info) => info
            .title
            .value()
            .cloned()
            .map(Event::AcpSessionTitle)
            .into_iter()
            .collect(),
        SessionUpdate::UsageUpdate(usage) => {
            let token_usage = TokenUsage {
                prompt_tokens: Some(usage.used as i32),
                completion_tokens: None,
                total_tokens: Some(usage.size as i32),
                cached_tokens: None,
                reasoning_tokens: None,
            };
            vec![Event::AcpUsageUpdate(token_usage)]
        }
        _ => Vec::new(),
    }
}

fn tool_call_info_from_acp(tc: &ToolCall) -> ToolCallInfo {
    let args = tc
        .raw_input
        .clone()
        .unwrap_or(Value::Object(Default::default()));
    ToolCallInfo {
        call_id: tc.tool_call_id.0.to_string(),
        fn_name: tc.title.clone(),
        fn_arguments: args,
    }
}

/// Compact summary of a tool-call update for the conversation view.
#[derive(Clone, Debug)]
pub struct ToolCallUpdateSummary {
    pub call_id: String,
    pub title: Option<String>,
    pub status: Option<String>,
    pub content_text: Option<String>,
}

fn tool_call_update_summary(update: ToolCallUpdate) -> ToolCallUpdateSummary {
    let fields = update.fields;
    let status = fields.status.as_ref().map(|s| format!("{s:?}"));
    let content_text = fields.content.as_ref().and_then(|blocks| {
        let texts: Vec<String> = blocks.iter().filter_map(tool_call_content_text).collect();
        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        }
    });
    ToolCallUpdateSummary {
        call_id: update.tool_call_id.0.to_string(),
        title: fields.title.clone(),
        status,
        content_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        ContentChunk, SessionUpdate, TextContent, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
    };

    #[test]
    fn agent_message_chunk_maps_to_stream_chunk() {
        let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("hello"),
        )));
        let events = session_update_to_events(update);
        assert!(matches!(events.as_slice(), [Event::StreamChunk(t)] if t == "hello"));
    }

    #[test]
    fn content_block_text_extracts_plain_text() {
        let block = ContentBlock::Text(TextContent::new("hi"));
        assert_eq!(content_block_text(&block).as_deref(), Some("hi"));
    }

    #[test]
    fn tool_call_update_summary_includes_call_id() {
        let update = ToolCallUpdate::new(ToolCallId::new("tc-1"), ToolCallUpdateFields::new());
        let summary = tool_call_update_summary(update);
        assert_eq!(summary.call_id, "tc-1");
    }
}
