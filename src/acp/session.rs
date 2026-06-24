//! Session identity shared by local (SQLite) and ACP-backed sessions.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Opaque session reference used by the session list / load / delete actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRef {
    /// Conversation stored in ozma.db.
    Local(Uuid),
    /// Session owned by an ACP agent.
    Acp(String),
}

impl SessionRef {
    pub fn as_str(&self) -> String {
        match self {
            SessionRef::Local(id) => id.to_string(),
            SessionRef::Acp(id) => id.clone(),
        }
    }
}

/// Row shown in the session list overlay (source-agnostic).
#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: SessionRef,
    pub title: String,
    pub provider: String,
    pub model: String,
    pub message_count: usize,
    pub updated_at: DateTime<Utc>,
    /// Working directory when known (ACP sessions); `None` for local SQLite rows.
    pub cwd: Option<String>,
}
