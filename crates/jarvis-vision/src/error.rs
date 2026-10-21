pub type Result<T, E = VisionError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum VisionError {
    /// Not possible on this platform or session.
    #[error("{0}")]
    Unsupported(String),
    /// The user declined in the system dialog.
    #[error("the capture was cancelled")]
    Cancelled,
    #[error("{0}")]
    NotFound(String),
    #[error("capture failed: {0}")]
    Capture(String),
    #[error("{0}")]
    InvalidImage(String),
    #[error("I/O error while {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl VisionError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}
