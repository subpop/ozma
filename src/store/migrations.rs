use rusqlite_migration::{M, Migrations};

/// All schema migrations, applied sequentially via `user_version` pragma.
///
/// This is a single flat migration covering the complete schema.
/// Pre-release: old databases are discarded rather than incrementally migrated.
const MIGRATION_ARRAY: &[M] = &[M::up(
    "CREATE TABLE conversations (
            id              TEXT PRIMARY KEY,
            title           TEXT NOT NULL,
            provider        TEXT NOT NULL DEFAULT '',
            model           TEXT NOT NULL DEFAULT '',
            temperature     REAL,
            top_p           REAL,
            max_tokens      INTEGER,
            persona         TEXT NOT NULL DEFAULT 'ozma',
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        CREATE TABLE messages (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            role            TEXT NOT NULL,
            content         TEXT NOT NULL,
            timestamp       INTEGER NOT NULL,
            compacted       INTEGER NOT NULL DEFAULT 0,
            tool_calls      TEXT,
            tool_result     TEXT,
            prompt_tokens   INTEGER,
            completion_tokens INTEGER,
            total_tokens    INTEGER,
            cached_tokens   INTEGER,
            reasoning_tokens INTEGER,
            FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
        );

        CREATE TABLE attachments (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            message_id      TEXT NOT NULL,
            mime_type       TEXT NOT NULL,
            data            BLOB NOT NULL,
            created_at      INTEGER NOT NULL,
            FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
            FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
        );

        CREATE INDEX idx_messages_conversation
            ON messages(conversation_id, timestamp);

        CREATE INDEX idx_conversations_updated
            ON conversations(updated_at DESC);

        CREATE INDEX idx_attachments_message
            ON attachments(message_id);",
)];

pub const MIGRATIONS: Migrations<'_> = Migrations::from_slice(MIGRATION_ARRAY);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        MIGRATIONS.validate().unwrap();
    }
}
