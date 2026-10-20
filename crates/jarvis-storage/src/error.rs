pub type Result<T, E = StorageError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("I/O error while {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("the database {path} is damaged or is not a JARVIS database")]
    Corrupt { path: String },

    #[error(
        "the database was created by a newer JARVIS (schema version {found}, this build supports {supported})"
    )]
    NewerSchema { found: u32, supported: u32 },

    #[error("database migration {version} differs from the one applied to this database")]
    MigrationChanged { version: u32 },

    #[error("SQLite was built without full-text search (FTS5)")]
    MissingFts5,

    #[error("internal storage error: {0}")]
    Internal(String),
}

impl StorageError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// SQLite reports the file as damaged or not a database at all.
    pub(crate) fn is_corruption(err: &rusqlite::Error) -> bool {
        matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        )
    }
}
