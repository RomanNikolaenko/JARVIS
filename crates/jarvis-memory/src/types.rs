//! Memory data types shared with the UI (exported to TypeScript).

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_NOTE_CHARS: usize = 4_000;
pub const MAX_FACT_CHARS: usize = 2_000;
pub const MAX_PROJECT_NAME_CHARS: usize = 100;
pub const MAX_PROJECT_DESCRIPTION_CHARS: usize = 2_000;
pub const MAX_PROJECT_INSTRUCTIONS_CHARS: usize = 8_000;
pub const MAX_MESSAGE_CHARS: usize = 32_000;
pub const MAX_FACTS: u32 = 10_000;
pub const MAX_PROJECTS: u32 = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct Conversation {
    pub id: Uuid,
    pub title: String,
    pub project_id: Option<Uuid>,
    /// Working memory the user pinned for this conversation; always sent to
    /// the model.
    pub pinned_note: String,
    /// Kept only in memory: never written to disk, gone after a restart.
    pub temporary: bool,
    pub message_count: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Changes to a conversation; `None` leaves a field unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct ConversationPatch {
    pub title: Option<String>,
    /// Moves the conversation into this project.
    pub project_id: Option<Uuid>,
    /// Removes the conversation from its project (wins over `project_id`).
    #[serde(default)]
    pub clear_project: bool,
    pub pinned_note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    pub(crate) fn parse(value: &str) -> Self {
        if value == "assistant" {
            Self::Assistant
        } else {
            Self::User
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub enum MessageStatus {
    Complete,
    /// Stopped by the user; the partial answer is kept.
    Cancelled,
}

impl MessageStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(value: &str) -> Self {
        if value == "cancelled" {
            Self::Cancelled
        } else {
            Self::Complete
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct StoredMessage {
    pub id: Uuid,
    /// Position within the conversation (database row id, or a counter for
    /// temporary conversations).
    #[serde(skip)]
    #[ts(skip)]
    pub seq: i64,
    pub role: MessageRole,
    pub content: String,
    pub status: MessageStatus,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    /// Estimated token count.
    pub tokens: u32,
    pub created_at_ms: i64,
}

/// A message to append to a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    pub id: Uuid,
    pub role: MessageRole,
    pub content: String,
    pub status: MessageStatus,
    pub provider_id: Option<String>,
    pub model: Option<String>,
}

/// Running summary of the older part of a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct ConversationSummary {
    pub content: String,
    /// Messages the summary stands for.
    pub covered_messages: u32,
    pub model: Option<String>,
    pub updated_at_ms: i64,
    #[serde(skip)]
    #[ts(skip)]
    pub covers_until: i64,
    #[serde(skip)]
    #[ts(skip)]
    pub tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct ConversationDetail {
    pub conversation: Conversation,
    pub messages: Vec<StoredMessage>,
    pub summary: Option<ConversationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// Sent to the model in every conversation of the project.
    pub instructions: String,
    pub fact_count: u32,
    pub conversation_count: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct ProjectInput {
    /// `None` creates a project.
    pub id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub instructions: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub enum FactSource {
    /// Written by the user on the Memory screen.
    User,
    /// Saved from a chat message.
    Conversation,
    /// Came from an imported file.
    Import,
}

impl FactSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Conversation => "conversation",
            Self::Import => "import",
        }
    }

    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "conversation" => Self::Conversation,
            "import" => Self::Import,
            _ => Self::User,
        }
    }
}

/// A long-term fact (semantic memory), global or scoped to a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct Fact {
    pub id: Uuid,
    pub content: String,
    pub project_id: Option<Uuid>,
    /// Always included in the context, not only when relevant.
    pub pinned: bool,
    pub source: FactSource,
    pub source_conversation_id: Option<Uuid>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct FactInput {
    /// `None` creates a fact.
    pub id: Option<Uuid>,
    pub content: String,
    pub project_id: Option<Uuid>,
    pub pinned: bool,
    pub source_conversation_id: Option<Uuid>,
}

/// Something JARVIS did (episodic memory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct Episode {
    pub id: Uuid,
    pub occurred_at_ms: i64,
    pub category: String,
    pub level: String,
    pub summary: String,
}

/// Exclusive history cursor. The ID breaks ties between equal timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct EpisodeCursor {
    pub occurred_at_ms: i64,
    pub id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub enum MemoryKind {
    Message,
    Fact,
    Episode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct SearchQuery {
    pub text: String,
    /// Empty means all kinds.
    pub kinds: Vec<MemoryKind>,
    pub limit: u32,
}

/// Highlight markers in [`SearchHit::snippet`]: text between them matched.
pub const HIGHLIGHT_START: char = '\u{2}';
pub const HIGHLIGHT_END: char = '\u{3}';

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct SearchHit {
    pub kind: MemoryKind,
    pub id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub conversation_title: Option<String>,
    pub role: Option<MessageRole>,
    /// Excerpt; matches are wrapped in U+0002 … U+0003.
    pub snippet: String,
    pub score: f32,
    pub matched_text: bool,
    pub matched_semantic: bool,
    pub created_at_ms: i64,
}

/// How the context of one request was built (shown in the chat).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct ContextReport {
    /// Context window of the model, in tokens.
    pub window_tokens: u32,
    /// Tokens available for the prompt after reserving room for the answer.
    pub budget_tokens: u32,
    /// Estimated prompt size.
    pub used_tokens: u32,
    pub system_tokens: u32,
    pub project_tokens: u32,
    pub note_tokens: u32,
    pub memory_tokens: u32,
    pub summary_tokens: u32,
    pub history_tokens: u32,
    /// Earlier messages sent verbatim.
    pub history_included: u32,
    /// Earlier messages left out and not covered by a summary.
    pub history_omitted: u32,
    /// Earlier messages represented by the conversation summary.
    pub summarized_messages: u32,
    pub facts_included: u32,
    /// Excerpts from other conversations.
    pub recalls_included: u32,
    pub project: Option<String>,
    /// The older part of the conversation should be summarized.
    pub needs_compaction: bool,
    /// Memory was not used, e.g. for a cloud model without permission.
    pub memory_skipped: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub enum EmbeddingState {
    /// No embedding model is configured or reachable; search is text-only.
    Unavailable,
    Indexing,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct EmbeddingStatus {
    pub state: EmbeddingState,
    /// `provider/model` of the vectors in use.
    pub model: Option<String>,
    pub indexed: u32,
    pub pending: u32,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct MemoryCounts {
    pub conversations: u32,
    pub messages: u32,
    pub facts: u32,
    pub projects: u32,
    pub episodes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct MemoryStatus {
    /// The database is open.
    pub available: bool,
    pub error: Option<String>,
    /// Saving is on (settings).
    pub saving_enabled: bool,
    pub database_path: Option<String>,
    pub size_bytes: u64,
    pub schema_version: u32,
    pub counts: MemoryCounts,
    pub embedding: EmbeddingStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "memory.ts")]
pub struct TransferReport {
    pub path: String,
    pub counts: MemoryCounts,
    /// Items already present (same id) and therefore not imported.
    pub skipped: u32,
}
