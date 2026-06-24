use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use color_eyre::eyre::{Result, eyre};
use rusqlite::{Connection, params};
use tracing::info;
use uuid::Uuid;

use crate::config::ModelParams;
use crate::model::conversation::Conversation;
use crate::model::message::{ContentPart, Message, Role, TokenUsage};
use crate::persona::Persona;

/// A summary of a conversation for the session list (without full message content).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ConversationSummary {
    pub id: Uuid,
    pub title: String,
    pub provider: String,
    pub model: String,
    pub message_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// SQLite-backed persistence for conversations and messages.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (or create) the database at `~/.local/share/ozma/ozma.db`.
    pub fn open() -> Result<Self> {
        let path = Self::db_path()?;

        // Ensure the parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut conn = Connection::open(&path)
            .map_err(|e| eyre!("Failed to open database at {:?}: {}", path, e))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys = ON;")?;

        super::migrations::MIGRATIONS
            .to_latest(&mut conn)
            .map_err(|e| eyre!("Failed to run migrations: {}", e))?;

        info!("Database opened at {:?}", path);
        Ok(Self { conn })
    }

    /// Open an in-memory database with migrations applied (for tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()
            .map_err(|e| eyre!("Failed to open in-memory database: {}", e))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys = ON;")?;

        super::migrations::MIGRATIONS
            .to_latest(&mut conn)
            .map_err(|e| eyre!("Failed to run migrations: {}", e))?;

        Ok(Self { conn })
    }

    /// Path to the database file.
    fn db_path() -> Result<PathBuf> {
        let data_dir = Self::data_dir()?;
        Ok(data_dir.join("ozma.db"))
    }

    /// Platform-appropriate data directory via the `dirs` crate.
    fn data_dir() -> Result<PathBuf> {
        let base = dirs::data_dir().ok_or_else(|| eyre!("Could not determine data directory"))?;
        Ok(base.join("ozma"))
    }

    // ── Conversation CRUD ──────────────────────────────────────────

    /// Save a new conversation (header only, no messages).
    /// All session config (provider, model, params) is read from the Conversation.
    pub fn save_conversation(&self, conversation: &Conversation) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO conversations
             (id, title, provider, model, temperature, top_p, max_tokens, persona, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                conversation.id.to_string(),
                conversation.title,
                conversation.provider,
                conversation.model,
                conversation.param_overrides.temperature,
                conversation.param_overrides.top_p,
                conversation.param_overrides.max_tokens,
                conversation.persona.name(),
                conversation.created_at.timestamp(),
                conversation.updated_at.timestamp(),
            ],
        )?;
        Ok(())
    }

    /// Update the conversation's title and updated_at timestamp.
    pub fn update_conversation_meta(&self, conversation: &Conversation) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                conversation.title,
                conversation.updated_at.timestamp(),
                conversation.id.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Persist the session's config (provider, model, params) to the database.
    pub fn update_conversation_config(&self, conversation: &Conversation) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations
             SET provider = ?1, model = ?2, temperature = ?3, top_p = ?4,
                 max_tokens = ?5, persona = ?6, updated_at = ?7
             WHERE id = ?8",
            params![
                conversation.provider,
                conversation.model,
                conversation.param_overrides.temperature,
                conversation.param_overrides.top_p,
                conversation.param_overrides.max_tokens,
                conversation.persona.name(),
                conversation.updated_at.timestamp(),
                conversation.id.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Delete a conversation and all its messages (via FK CASCADE).
    pub fn delete_conversation(&self, id: &Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM conversations WHERE id = ?1",
            params![id.to_string()],
        )?;
        info!("Deleted conversation {}", id);
        Ok(())
    }

    /// List all conversations, most recently updated first.
    pub fn list_conversations(&self) -> Result<Vec<ConversationSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.title, c.provider, c.model, c.created_at, c.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id)
             FROM conversations c
             ORDER BY c.updated_at DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let title: String = row.get(1)?;
            let provider: String = row.get(2)?;
            let model: String = row.get(3)?;
            let created_ts: i64 = row.get(4)?;
            let updated_ts: i64 = row.get(5)?;
            let message_count: i64 = row.get(6)?;

            Ok(ConversationSummary {
                id: Uuid::parse_str(&id_str).unwrap_or_default(),
                title,
                provider,
                model,
                message_count: message_count as usize,
                created_at: Utc.timestamp_opt(created_ts, 0).unwrap(),
                updated_at: Utc.timestamp_opt(updated_ts, 0).unwrap(),
            })
        })?;

        let mut summaries = Vec::new();
        for row in rows {
            summaries.push(row?);
        }
        Ok(summaries)
    }

    /// Load a full conversation with all its messages.
    pub fn load_conversation(&self, id: &Uuid) -> Result<Option<Conversation>> {
        let id_str = id.to_string();

        // Load conversation header (including session config)
        let mut stmt = self.conn.prepare(
            "SELECT id, title, provider, model, temperature, top_p, max_tokens,
                    persona, created_at, updated_at
             FROM conversations WHERE id = ?1",
        )?;

        let conv_result = stmt.query_row(params![id_str], |row| {
            let id_str: String = row.get(0)?;
            let title: String = row.get(1)?;
            let provider: String = row.get(2)?;
            let model: String = row.get(3)?;
            let temperature: Option<f64> = row.get(4)?;
            let top_p: Option<f64> = row.get(5)?;
            let max_tokens: Option<u32> = row.get(6)?;
            let persona_str: String = row.get(7)?;
            let created_ts: i64 = row.get(8)?;
            let updated_ts: i64 = row.get(9)?;

            Ok((
                id_str,
                title,
                provider,
                model,
                temperature,
                top_p,
                max_tokens,
                persona_str,
                created_ts,
                updated_ts,
            ))
        });

        let (
            conv_id_str,
            title,
            provider,
            model,
            temperature,
            top_p,
            max_tokens,
            persona_str,
            created_ts,
            updated_ts,
        ) = match conv_result {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        // Load messages
        let mut msg_stmt = self.conn.prepare(
            "SELECT id, role, content, timestamp, compacted, tool_calls, tool_result,
                    prompt_tokens, completion_tokens, total_tokens, cached_tokens, reasoning_tokens
             FROM messages
             WHERE conversation_id = ?1
             ORDER BY timestamp ASC",
        )?;

        let messages: Vec<Message> = msg_stmt
            .query_map(params![id_str], |row| {
                let msg_id_str: String = row.get(0)?;
                let role_str: String = row.get(1)?;
                let content_str: String = row.get(2)?;
                let timestamp_val: i64 = row.get(3)?;
                let compacted: i32 = row.get(4)?;
                let tool_calls_str: Option<String> = row.get(5)?;
                let tool_result_str: Option<String> = row.get(6)?;
                let prompt_tokens: Option<i32> = row.get(7)?;
                let completion_tokens: Option<i32> = row.get(8)?;
                let total_tokens: Option<i32> = row.get(9)?;
                let cached_tokens: Option<i32> = row.get(10)?;
                let reasoning_tokens: Option<i32> = row.get(11)?;

                // Deserialize content from JSON. Fall back to wrapping raw text
                // if the column contains plain text (backwards compat during dev).
                let content: Vec<ContentPart> = serde_json::from_str(&content_str)
                    .unwrap_or_else(|_| vec![ContentPart::Text { text: content_str }]);

                let tool_calls = tool_calls_str
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();

                let tool_result = tool_result_str.and_then(|s| serde_json::from_str(&s).ok());

                let usage = if prompt_tokens.is_some()
                    || completion_tokens.is_some()
                    || total_tokens.is_some()
                {
                    Some(TokenUsage {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cached_tokens,
                        reasoning_tokens,
                    })
                } else {
                    None
                };

                Ok(Message {
                    id: Uuid::parse_str(&msg_id_str).unwrap_or_default(),
                    role: match role_str.as_str() {
                        "system" => Role::System,
                        "user" => Role::User,
                        "assistant" => Role::Assistant,
                        "tool" => Role::Tool,
                        _ => Role::User,
                    },
                    content,
                    timestamp: Utc.timestamp_opt(timestamp_val, 0).unwrap(),
                    streaming: false,
                    compacted: compacted != 0,
                    tool_calls,
                    tool_result,
                    usage,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut conversation = Conversation::new();
        conversation.id = Uuid::parse_str(&conv_id_str).unwrap_or_default();
        conversation.title = title;
        conversation.provider = provider;
        conversation.model = model;
        conversation.param_overrides = ModelParams {
            temperature,
            top_p,
            max_tokens,
        };
        conversation.persona = Persona::from_name(&persona_str);
        conversation.messages = messages;
        conversation.created_at = Utc.timestamp_opt(created_ts, 0).unwrap();
        conversation.updated_at = Utc.timestamp_opt(updated_ts, 0).unwrap();
        conversation.rebuild_session_usage();

        Ok(Some(conversation))
    }

    // ── Message CRUD ───────────────────────────────────────────────

    /// Save a single message to the database.
    /// Content is serialized as JSON.
    pub fn save_message(&self, conversation_id: &Uuid, message: &Message) -> Result<()> {
        let content_json = serde_json::to_string(&message.content)
            .map_err(|e| eyre!("Failed to serialize message content: {}", e))?;

        let tool_calls_json: Option<String> = if message.tool_calls.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&message.tool_calls)
                    .map_err(|e| eyre!("Failed to serialize tool_calls: {}", e))?,
            )
        };

        let tool_result_json: Option<String> = message
            .tool_result
            .as_ref()
            .map(|r| serde_json::to_string(r))
            .transpose()
            .map_err(|e| eyre!("Failed to serialize tool_result: {}", e))?;

        let (prompt_tokens, completion_tokens, total_tokens, cached_tokens, reasoning_tokens) =
            match &message.usage {
                Some(u) => (
                    u.prompt_tokens,
                    u.completion_tokens,
                    u.total_tokens,
                    u.cached_tokens,
                    u.reasoning_tokens,
                ),
                None => (None, None, None, None, None),
            };

        self.conn.execute(
            "INSERT OR REPLACE INTO messages
             (id, conversation_id, role, content, timestamp, compacted, tool_calls, tool_result,
              prompt_tokens, completion_tokens, total_tokens, cached_tokens, reasoning_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                message.id.to_string(),
                conversation_id.to_string(),
                match message.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                },
                content_json,
                message.timestamp.timestamp(),
                message.compacted as i32,
                tool_calls_json,
                tool_result_json,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_tokens,
                reasoning_tokens,
            ],
        )?;
        Ok(())
    }

    /// Mark messages as compacted in the database.
    pub fn mark_messages_compacted(&self, message_ids: &[Uuid]) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let placeholders: Vec<&str> = message_ids.iter().map(|_| "?").collect();
        let sql = format!(
            "UPDATE messages SET compacted = 1 WHERE id IN ({})",
            placeholders.join(", ")
        );
        let params: Vec<String> = message_ids.iter().map(|id| id.to_string()).collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        self.conn.execute(&sql, param_refs.as_slice())?;
        Ok(())
    }

    // ── Attachment CRUD ────────────────────────────────────────────

    /// Save an attachment (image blob) to the database.
    pub fn save_attachment(
        &self,
        id: &Uuid,
        conversation_id: &Uuid,
        message_id: &Uuid,
        mime_type: &str,
        data: &[u8],
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO attachments (id, conversation_id, message_id, mime_type, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.to_string(),
                conversation_id.to_string(),
                message_id.to_string(),
                mime_type,
                data,
                Utc::now().timestamp(),
            ],
        )?;
        Ok(())
    }

    /// Load a single attachment by ID. Returns (mime_type, data).
    pub fn load_attachment(&self, id: &Uuid) -> Result<Option<(String, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT mime_type, data FROM attachments WHERE id = ?1")?;

        let result = stmt.query_row(params![id.to_string()], |row| {
            let mime_type: String = row.get(0)?;
            let data: Vec<u8> = row.get(1)?;
            Ok((mime_type, data))
        });

        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load all attachments referenced by a set of IDs.
    /// Returns a map from attachment UUID to (mime_type, data).
    pub fn load_attachments(&self, ids: &[Uuid]) -> Result<HashMap<Uuid, (String, Vec<u8>)>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut map = HashMap::new();
        for id in ids {
            if let Some(attachment) = self.load_attachment(id)? {
                map.insert(*id, attachment);
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::message::Message;

    fn sample_conversation() -> Conversation {
        let mut conv = Conversation::new();
        conv.title = "Test session".to_string();
        conv.provider = "openai".to_string();
        conv.model = "gpt-4o".to_string();
        conv
    }

    #[test]
    fn conversation_crud() {
        let store = Store::open_in_memory().unwrap();
        let mut conv = sample_conversation();
        store.save_conversation(&conv).unwrap();

        let list = store.list_conversations().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Test session");

        conv.title = "Renamed".to_string();
        store.update_conversation_meta(&conv).unwrap();

        let loaded = store.load_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.title, "Renamed");
        assert_eq!(loaded.provider, "openai");
        assert_eq!(loaded.model, "gpt-4o");

        store.delete_conversation(&conv.id).unwrap();
        assert!(store.load_conversation(&conv.id).unwrap().is_none());
        assert!(store.list_conversations().unwrap().is_empty());
    }

    #[test]
    fn save_message_and_mark_compacted() {
        let store = Store::open_in_memory().unwrap();
        let conv = sample_conversation();
        store.save_conversation(&conv).unwrap();

        let msg = Message::user("hello");
        store.save_message(&conv.id, &msg).unwrap();

        let loaded = store.load_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].text_content(), "hello");
        assert!(!loaded.messages[0].compacted);

        store.mark_messages_compacted(&[msg.id]).unwrap();
        let loaded = store.load_conversation(&conv.id).unwrap().unwrap();
        assert!(loaded.messages[0].compacted);
    }

    #[test]
    fn attachment_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let conv = sample_conversation();
        store.save_conversation(&conv).unwrap();

        let msg = Message::user("with image");
        store.save_message(&conv.id, &msg).unwrap();

        let att_id = Uuid::new_v4();
        let data = b"fake-png-bytes";
        store
            .save_attachment(&att_id, &conv.id, &msg.id, "image/png", data)
            .unwrap();

        let (mime, bytes) = store.load_attachment(&att_id).unwrap().unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, data);

        let map = store.load_attachments(&[att_id]).unwrap();
        assert_eq!(map.len(), 1);
        assert!(store.load_attachment(&Uuid::nil()).unwrap().is_none());
    }

    #[test]
    fn update_conversation_config() {
        let store = Store::open_in_memory().unwrap();
        let mut conv = sample_conversation();
        store.save_conversation(&conv).unwrap();

        conv.provider = "anthropic".to_string();
        conv.model = "claude".to_string();
        conv.param_overrides.temperature = Some(0.2);
        conv.persona = Persona::OzCode;
        store.update_conversation_config(&conv).unwrap();

        let loaded = store.load_conversation(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.provider, "anthropic");
        assert_eq!(loaded.model, "claude");
        assert_eq!(loaded.param_overrides.temperature, Some(0.2));
        assert_eq!(loaded.persona, Persona::OzCode);
    }
}
