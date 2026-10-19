use std::path::PathBuf;

/// Errors of the local runtime.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("the llama.cpp runtime is not installed")]
    NotInstalled,

    #[error("{0}")]
    InvalidInput(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    PermissionDenied(String),

    #[error("not a GGUF model file: {0}")]
    InvalidModel(String),

    #[error(
        "not enough free memory: the model needs about {needed_mib} MiB, {available_mib} MiB are available; choose a smaller model, reduce the context size or close other applications"
    )]
    InsufficientMemory { needed_mib: u64, available_mib: u64 },

    #[error("not enough disk space: {needed_mib} MiB needed, {available_mib} MiB free")]
    InsufficientDisk { needed_mib: u64, available_mib: u64 },

    #[error("integrity check failed for {file}: expected SHA-256 {expected}, got {actual}")]
    IntegrityMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    #[error(
        "the runtime binary {0} changed since it was installed; reinstall it or select it again"
    )]
    BinaryChanged(PathBuf),

    #[error("download failed: {0}")]
    Download(String),

    #[error("{0}")]
    Unavailable(String),

    #[error("the llama.cpp server exited during startup ({status}). Last log lines:\n{log}")]
    StartupFailed { status: String, log: String },

    #[error("the llama.cpp server did not become ready within {0} seconds")]
    StartupTimeout(u64),

    #[error("operation cancelled")]
    Cancelled,

    #[error("I/O error while {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("internal error: {0}")]
    Internal(String),
}

impl RuntimeError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, RuntimeError>;
