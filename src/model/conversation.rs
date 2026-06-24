use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::message::{Message, Role, TokenUsage};
use crate::config::ModelParams;
use crate::persona::Persona;

/// Default context window sizes for common model families.
/// These are conservative estimates used when the exact limit is unknown.
const DEFAULT_CONTEXT_WINDOW: u32 = 128_000;

/// The percentage of context window usage at which auto-compaction is suggested.
const AUTO_COMPACT_THRESHOLD: f64 = 0.75;

/// A conversation session containing a sequence of messages.
#[derive(Clone, Debug)]
pub struct Conversation {
    /// Unique identifier for this conversation.
    pub id: Uuid,
    /// Human-readable title (derived from first message or auto-generated).
    pub title: String,
    /// The messages in chronological order.
    pub messages: Vec<Message>,
    /// When this conversation was created.
    pub created_at: DateTime<Utc>,
    /// When this conversation was last updated.
    pub updated_at: DateTime<Utc>,
    /// The context window size for the active model (in tokens).
    pub context_window: u32,
    /// Per-conversation parameter overrides (persisted to DB).
    pub param_overrides: ModelParams,
    /// The provider name for this session.
    pub provider: String,
    /// The model identifier for this session.
    pub model: String,
    /// Cumulative token usage for this session (all API calls).
    pub session_usage: TokenUsage,
    /// The prompt_tokens from the most recent API response (actual context size in tokens).
    /// Used for accurate context % calculation instead of heuristic estimates.
    pub last_prompt_tokens: Option<i32>,
    /// Cumulative cost for this session in USD (computed from pricing catalog).
    pub session_cost: f64,
    /// The active persona for this conversation.
    pub persona: Persona,
}

impl Conversation {
    /// Create a new empty conversation.
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title: String::from("New conversation"),
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
            context_window: DEFAULT_CONTEXT_WINDOW,
            param_overrides: ModelParams::default(),
            provider: String::new(),
            model: String::new(),
            session_usage: TokenUsage::default(),
            last_prompt_tokens: None,
            session_cost: 0.0,
            persona: Persona::default(),
        }
    }

    /// Add a user message and return its index.
    pub fn add_user_message(&mut self, content: impl Into<String>) -> usize {
        let msg = Message::user(content);
        self.messages.push(msg);
        self.updated_at = Utc::now();
        self.messages.len() - 1
    }

    /// Start a new streaming assistant message and return its index.
    pub fn start_assistant_message(&mut self) -> usize {
        let msg = Message::assistant_streaming();
        self.messages.push(msg);
        self.updated_at = Utc::now();
        self.messages.len() - 1
    }

    /// Append a chunk to the latest assistant message.
    pub fn append_to_last(&mut self, chunk: &str) {
        if let Some(msg) = self.messages.last_mut() {
            if msg.role == Role::Assistant && msg.streaming {
                msg.append(chunk);
                self.updated_at = Utc::now();
            }
        }
    }

    /// Mark the latest assistant message as complete.
    pub fn complete_last(&mut self) {
        if let Some(msg) = self.messages.last_mut() {
            if msg.role == Role::Assistant && msg.streaming {
                msg.complete();
                self.updated_at = Utc::now();
            }
        }
    }

    // ── Token tracking ─────────────────────────────────────────────

    /// Estimate total tokens for all non-compacted messages (the active context).
    pub fn active_token_count(&self) -> u32 {
        self.messages
            .iter()
            .filter(|m| !m.compacted)
            .map(|m| m.estimate_tokens())
            .sum()
    }

    /// Estimate total tokens for all messages (including compacted).
    #[allow(dead_code)]
    pub fn total_token_count(&self) -> u32 {
        self.messages.iter().map(|m| m.estimate_tokens()).sum()
    }

    /// Context window usage as a fraction (0.0 to 1.0+).
    /// Uses actual prompt_tokens from the last API response when available,
    /// falls back to heuristic estimate otherwise.
    pub fn context_usage(&self) -> f64 {
        if self.context_window == 0 {
            return 0.0;
        }
        let tokens = if let Some(pt) = self.last_prompt_tokens {
            pt as u32
        } else {
            self.active_token_count()
        };
        tokens as f64 / self.context_window as f64
    }

    /// Whether the conversation should suggest compaction.
    pub fn should_suggest_compaction(&self) -> bool {
        // Need at least 4 non-compacted messages to compact (otherwise nothing useful to summarize)
        let non_compacted = self.messages.iter().filter(|m| !m.compacted).count();
        non_compacted >= 4 && self.context_usage() >= AUTO_COMPACT_THRESHOLD
    }

    /// Record usage from an API call: accumulate into session totals and
    /// update the last_prompt_tokens for accurate context % tracking.
    pub fn add_usage(&mut self, usage: &TokenUsage) {
        self.session_usage.add(usage);
        if usage.prompt_tokens.is_some() {
            self.last_prompt_tokens = usage.prompt_tokens;
        }
    }

    /// Rebuild session_usage and last_prompt_tokens from persisted per-message usage.
    /// Called after loading a conversation from the database.
    pub fn rebuild_session_usage(&mut self) {
        self.session_usage = TokenUsage::default();
        self.last_prompt_tokens = None;
        for msg in &self.messages {
            if let Some(ref usage) = msg.usage {
                self.session_usage.add(usage);
                if usage.prompt_tokens.is_some() {
                    self.last_prompt_tokens = usage.prompt_tokens;
                }
            }
        }
    }

    // ── Compaction ─────────────────────────────────────────────────

    /// Get the non-compacted messages that would be sent to the LLM as active context.
    #[allow(dead_code)]
    pub fn active_messages(&self) -> Vec<&Message> {
        self.messages.iter().filter(|m| !m.compacted).collect()
    }

    /// Get the messages eligible for compaction (older non-compacted messages,
    /// keeping the most recent N messages un-compacted).
    /// Returns (messages_to_compact, messages_to_keep).
    pub fn compaction_split(&self, keep_recent: usize) -> (Vec<&Message>, Vec<&Message>) {
        let non_compacted: Vec<&Message> = self.messages.iter().filter(|m| !m.compacted).collect();

        if non_compacted.len() <= keep_recent {
            return (vec![], non_compacted);
        }

        let split_point = non_compacted.len() - keep_recent;
        let to_compact = non_compacted[..split_point].to_vec();
        let to_keep = non_compacted[split_point..].to_vec();
        (to_compact, to_keep)
    }

    /// Mark messages as compacted by their IDs and insert a summary message
    /// at the position of the first compacted message.
    pub fn apply_compaction(&mut self, compacted_ids: &[Uuid], summary: String) {
        // Find the index of the first message being compacted
        let first_compacted_idx = self
            .messages
            .iter()
            .position(|m| compacted_ids.contains(&m.id));

        // Mark all specified messages as compacted
        for msg in &mut self.messages {
            if compacted_ids.contains(&msg.id) {
                msg.compacted = true;
            }
        }

        // Insert a system summary message at the position of the first compacted message
        if let Some(idx) = first_compacted_idx {
            let summary_msg = Message::system(format!(
                "[Context compacted — {} messages summarized]\n\n{}",
                compacted_ids.len(),
                summary
            ));
            self.messages.insert(idx, summary_msg);
        }

        self.updated_at = Utc::now();
    }

    /// Count how many messages have been compacted.
    #[allow(dead_code)]
    pub fn compacted_count(&self) -> usize {
        self.messages.iter().filter(|m| m.compacted).count()
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: i32, completion: i32) -> TokenUsage {
        TokenUsage {
            prompt_tokens: Some(prompt),
            completion_tokens: Some(completion),
            total_tokens: Some(prompt + completion),
            cached_tokens: None,
            reasoning_tokens: None,
        }
    }

    #[test]
    fn context_usage_zero_window_and_prompt_tokens() {
        let mut conv = Conversation::new();
        conv.context_window = 0;
        assert_eq!(conv.context_usage(), 0.0);

        conv.context_window = 1000;
        conv.add_user_message("hello"); // heuristic path
        let heuristic = conv.context_usage();
        assert!(heuristic > 0.0);

        conv.last_prompt_tokens = Some(500);
        assert!((conv.context_usage() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn should_suggest_compaction_needs_messages_and_threshold() {
        let mut conv = Conversation::new();
        conv.context_window = 100;
        conv.last_prompt_tokens = Some(80); // 0.8 >= 0.75
        for _ in 0..3 {
            conv.add_user_message("x");
        }
        assert!(!conv.should_suggest_compaction());
        conv.add_user_message("y");
        assert!(conv.should_suggest_compaction());
    }

    #[test]
    fn compaction_split_and_apply() {
        let mut conv = Conversation::new();
        let ids: Vec<Uuid> = (0..5)
            .map(|i| {
                let idx = conv.add_user_message(format!("m{i}"));
                conv.messages[idx].id
            })
            .collect();

        let (to_compact, to_keep) = conv.compaction_split(2);
        assert_eq!(to_compact.len(), 3);
        assert_eq!(to_keep.len(), 2);

        let compact_ids: Vec<Uuid> = to_compact.iter().map(|m| m.id).collect();
        conv.apply_compaction(&compact_ids, "summary text".into());

        assert_eq!(conv.compacted_count(), 3);
        assert!(
            conv.messages
                .iter()
                .any(|m| m.role == Role::System && m.text_content().contains("summary text"))
        );
        let first = &conv.messages[0];
        assert_eq!(first.role, Role::System);
        assert!(first.text_content().contains("3 messages summarized"));
        // kept messages remain non-compacted
        assert!(!conv.messages.iter().any(|m| m.id == ids[3] && m.compacted));
        assert!(!conv.messages.iter().any(|m| m.id == ids[4] && m.compacted));
    }

    #[test]
    fn compaction_split_when_fewer_than_keep() {
        let mut conv = Conversation::new();
        conv.add_user_message("a");
        conv.add_user_message("b");
        let (to_compact, to_keep) = conv.compaction_split(5);
        assert!(to_compact.is_empty());
        assert_eq!(to_keep.len(), 2);
    }

    #[test]
    fn add_usage_and_rebuild_session_usage() {
        let mut conv = Conversation::new();
        conv.add_usage(&usage(10, 5));
        assert_eq!(conv.session_usage.prompt_tokens, Some(10));
        assert_eq!(conv.last_prompt_tokens, Some(10));

        let idx = conv.start_assistant_message();
        conv.messages[idx].usage = Some(usage(20, 8));
        conv.messages[idx].complete();
        let idx2 = conv.start_assistant_message();
        conv.messages[idx2].usage = Some(usage(30, 2));
        conv.messages[idx2].complete();

        conv.rebuild_session_usage();
        assert_eq!(conv.session_usage.prompt_tokens, Some(50));
        assert_eq!(conv.session_usage.completion_tokens, Some(10));
        assert_eq!(conv.last_prompt_tokens, Some(30));
    }
}
