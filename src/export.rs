use std::fmt::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::conversation::Conversation;
use crate::model::message::{ContentPart, Role, ToolCallInfo, ToolResultInfo};

// ── Markdown export ────────────────────────────────────────────────

/// Export a conversation to a Markdown string.
pub fn to_markdown(conversation: &Conversation) -> String {
    let mut out = String::new();

    // Header
    writeln!(out, "# {}\n", conversation.title).unwrap();
    writeln!(
        out,
        "*{}/{} — {}*\n",
        conversation.provider,
        conversation.model,
        conversation.created_at.format("%Y-%m-%d %H:%M UTC")
    )
    .unwrap();
    writeln!(out, "---\n").unwrap();

    for msg in &conversation.messages {
        // Skip compacted messages (internal summarization artifacts)
        if msg.compacted {
            continue;
        }

        let heading = match msg.role {
            Role::System => "System",
            Role::User => "You",
            Role::Assistant => "Assistant",
            Role::Tool => {
                // Render tool result with function name if available
                if let Some(ref info) = msg.tool_result {
                    let icon = if info.success { "+" } else { "x" };
                    let _ = writeln!(out, "## [{icon}] Tool: {}\n", info.fn_name);
                } else {
                    let _ = writeln!(out, "## Tool\n");
                }
                // Render content
                write_content_parts(&mut out, &msg.content);
                writeln!(out, "\n---\n").unwrap();
                continue;
            }
        };

        writeln!(out, "## {heading}\n").unwrap();

        // Render tool calls on assistant messages
        for tc in &msg.tool_calls {
            writeln!(out, "**Tool call: `{}`**\n", tc.fn_name).unwrap();
            let args = serde_json::to_string_pretty(&tc.fn_arguments).unwrap_or_default();
            writeln!(out, "```json\n{args}\n```\n").unwrap();
        }

        write_content_parts(&mut out, &msg.content);
        writeln!(out, "\n---\n").unwrap();
    }

    out.trim_end().to_string()
}

/// Write content parts as markdown text.
fn write_content_parts(out: &mut String, parts: &[ContentPart]) {
    for part in parts {
        match part {
            ContentPart::Text { text } => {
                out.push_str(text);
            }
            ContentPart::Image { id, alt, .. } => {
                let alt_text = alt.as_deref().unwrap_or("image");
                let _ = write!(out, "![{alt_text}](attachment:{id})");
            }
        }
    }
}

// ── JSON export ────────────────────────────────────────────────────

/// A clean export schema, decoupled from internal model changes.
#[derive(Serialize)]
struct ExportedConversation {
    title: String,
    provider: String,
    model: String,
    created_at: DateTime<Utc>,
    messages: Vec<ExportedMessage>,
}

#[derive(Serialize)]
struct ExportedMessage {
    role: Role,
    content: String,
    timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ToolCallInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_result: Option<ToolResultInfo>,
}

/// Export a conversation to a pretty-printed JSON string.
pub fn to_json(conversation: &Conversation) -> Result<String, serde_json::Error> {
    let exported = ExportedConversation {
        title: conversation.title.clone(),
        provider: conversation.provider.clone(),
        model: conversation.model.clone(),
        created_at: conversation.created_at,
        messages: conversation
            .messages
            .iter()
            .filter(|m| !m.compacted)
            .map(|m| ExportedMessage {
                role: m.role.clone(),
                content: m.text_content(),
                timestamp: m.timestamp,
                tool_calls: m.tool_calls.clone(),
                tool_result: m.tool_result.clone(),
            })
            .collect(),
    };
    serde_json::to_string_pretty(&exported)
}

// ── File path helper ───────────────────────────────────────────────

/// Generate an export file path in the current directory from the conversation title.
pub fn export_file_path(title: &str, ext: &str) -> PathBuf {
    let sanitized: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .replace(' ', "_");
    let name = if sanitized.is_empty() {
        "conversation".to_string()
    } else {
        sanitized
    };
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
    PathBuf::from(format!("{name}_{timestamp}.{ext}"))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::model::message::{ContentPart, Message, Role, ToolCallInfo};

    fn sample_conversation() -> Conversation {
        let mut conv = Conversation::new();
        conv.title = "Hello World".to_string();
        conv.provider = "openai".to_string();
        conv.model = "gpt-4o".to_string();
        conv.created_at = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();

        let mut user = Message::user("Hi there");
        user.timestamp = conv.created_at;
        conv.messages.push(user);

        let mut assistant = Message::assistant_streaming();
        assistant.append("Hello!");
        assistant.complete();
        assistant.tool_calls.push(ToolCallInfo {
            call_id: "c1".to_string(),
            fn_name: "read_file".to_string(),
            fn_arguments: json!({"path": "src/main.rs"}),
        });
        assistant.timestamp = conv.created_at;
        conv.messages.push(assistant);

        let mut tool_ok = Message::tool_result(
            "c1".to_string(),
            "read_file".to_string(),
            vec![ContentPart::Text {
                text: "fn main() {}".to_string(),
            }],
            true,
        );
        tool_ok.timestamp = conv.created_at;
        conv.messages.push(tool_ok);

        let mut tool_err = Message::tool_result(
            "c2".to_string(),
            "write_file".to_string(),
            vec![ContentPart::Text {
                text: "permission denied".to_string(),
            }],
            false,
        );
        tool_err.timestamp = conv.created_at;
        conv.messages.push(tool_err);

        let mut system = Message::system("Be helpful");
        system.timestamp = conv.created_at;
        conv.messages.push(system);

        let img_id = Uuid::nil();
        let mut with_image = Message::user("see this");
        with_image.add_image(img_id, "image/png".to_string(), Some("diagram".to_string()));
        with_image.timestamp = conv.created_at;
        conv.messages.push(with_image);

        let mut compacted = Message::user("old stuff");
        compacted.compacted = true;
        compacted.timestamp = conv.created_at;
        conv.messages.push(compacted);

        conv
    }

    #[test]
    fn to_markdown_includes_header_and_roles() {
        let md = to_markdown(&sample_conversation());
        assert!(md.starts_with("# Hello World\n"));
        assert!(md.contains("*openai/gpt-4o — 2026-01-15 12:00 UTC*"));
        assert!(md.contains("## You\n"));
        assert!(md.contains("Hi there"));
        assert!(md.contains("## Assistant\n"));
        assert!(md.contains("Hello!"));
        assert!(md.contains("## System\n"));
        assert!(md.contains("Be helpful"));
        assert!(md.contains("**Tool call: `read_file`**"));
        assert!(md.contains("```json"));
        assert!(md.contains("## [+] Tool: read_file\n"));
        assert!(md.contains("## [x] Tool: write_file\n"));
        assert!(md.contains("![diagram](attachment:00000000-0000-0000-0000-000000000000)"));
        assert!(!md.contains("old stuff"));
    }

    #[test]
    fn to_json_filters_compacted_and_uses_text_content() {
        let json = to_json(&sample_conversation()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["title"], "Hello World");
        assert_eq!(value["provider"], "openai");
        assert_eq!(value["model"], "gpt-4o");
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 6);
        assert!(
            messages
                .iter()
                .all(|m| m["content"].as_str() != Some("old stuff"))
        );
        let image_msg = messages
            .iter()
            .find(|m| m["content"] == "see this")
            .unwrap();
        assert_eq!(image_msg["content"], "see this");
        assert_eq!(image_msg["role"], "User");
    }

    #[test]
    fn export_file_path_sanitizes_title() {
        let path = export_file_path("weird/title!", "md");
        let name = path.to_string_lossy();
        assert!(name.starts_with("weird_title_"));
        assert!(name.ends_with(".md"));
    }

    #[test]
    fn export_file_path_empty_title_uses_conversation() {
        let path = export_file_path("", "json");
        let name = path.to_string_lossy();
        assert!(name.starts_with("conversation_"));
        assert!(name.ends_with(".json"));
    }

    #[test]
    fn to_markdown_tool_without_result_info() {
        let mut conv = Conversation::new();
        conv.title = "T".to_string();
        conv.provider = "p".to_string();
        conv.model = "m".to_string();
        conv.created_at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut msg = Message::user("x");
        msg.role = Role::Tool;
        msg.tool_result = None;
        msg.content = vec![ContentPart::Text {
            text: "orphan".to_string(),
        }];
        conv.messages.push(msg);
        let md = to_markdown(&conv);
        assert!(md.contains("## Tool\n"));
        assert!(md.contains("orphan"));
    }
}
