use jarvis_runtime::RuntimeError;

pub type Result<T, E = VoiceError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    /// No audio device, or the device refused the stream.
    #[error("audio device: {0}")]
    Device(String),
    #[error("invalid audio: {0}")]
    InvalidAudio(String),
    /// A voice component (whisper.cpp, Piper) or model is missing.
    #[error("{0}")]
    NotInstalled(String),
    /// Not available on this platform or configuration.
    #[error("{0}")]
    Unsupported(String),
    /// Downloading or installing a package failed.
    #[error("{0}")]
    Install(String),
    /// A voice process or server failed.
    #[error("{0}")]
    Engine(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Busy(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timed out")]
    Timeout,
    #[error("I/O error while {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl VoiceError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

impl From<RuntimeError> for VoiceError {
    fn from(err: RuntimeError) -> Self {
        match err {
            RuntimeError::Cancelled => Self::Cancelled,
            RuntimeError::Io { context, source } => Self::Io { context, source },
            other => Self::Install(other.to_string()),
        }
    }
}
