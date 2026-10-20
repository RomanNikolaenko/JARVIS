use jarvis_storage::StorageError;

pub type Result<T, E = MemoryError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// The database could not be opened; temporary conversations still work.
    #[error("memory is unavailable: {0}")]
    Unavailable(String),

    /// Saving is turned off in the settings.
    #[error("{0}")]
    Disabled(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Invalid(String),

    #[error(
        "the message needs about {needed} tokens, but this model's context leaves room for {available}"
    )]
    MessageTooLong { needed: u32, available: u32 },

    #[error("embedding failed: {0}")]
    Embedding(String),

    #[error("internal memory error: {0}")]
    Internal(String),
}

impl From<jarvis_storage::rusqlite::Error> for MemoryError {
    fn from(err: jarvis_storage::rusqlite::Error) -> Self {
        Self::Storage(StorageError::Sqlite(err))
    }
}

impl From<serde_json::Error> for MemoryError {
    fn from(err: serde_json::Error) -> Self {
        Self::Invalid(format!("invalid memory data: {err}"))
    }
}
