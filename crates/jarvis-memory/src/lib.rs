//! JARVIS memory: the Memory Engine (conversation history, facts, projects,
//! episodes, full-text and vector search) and the Context Manager that fits
//! the relevant parts into a model's context window.
//!
//! Persistence is SQLite through `jarvis-storage`. AI calls (embeddings,
//! summaries) are made by the caller: this crate decides *what* to send,
//! the core decides *where* (provider choice and privacy policy).

pub mod context;
mod engine;
mod error;
mod export;
mod store;
pub mod text;
mod types;
pub mod vector;

pub use context::{
    AssembledContext, CompactionPlan, ContextInput, ContextWindow, DEFAULT_WINDOW_TOKENS,
    MemoryItem, MemoryItemKind,
};
pub use engine::{ContextRequest, DEFAULT_TITLE, MemoryEngine, Retrieval};
pub use error::{MemoryError, Result};
pub use export::{
    EXPORT_FORMAT, EXPORT_VERSION, ExportedConversation, ExportedSummary, ImportOutcome,
    MemoryExport,
};
pub use jarvis_storage::StorageError;
pub use types::*;
pub use vector::Embedder;
