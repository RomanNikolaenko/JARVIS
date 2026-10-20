use jarvis_ai::AiError;
use jarvis_memory::{MemoryError, StorageError};
use jarvis_runtime::RuntimeError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable, machine-readable error categories shared with the UI.
///
/// The UI branches on these codes, never on message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum ErrorCode {
    /// The request was malformed or violated a limit.
    Validation,
    NotFound,
    /// Blocked by a security or privacy policy.
    PermissionDenied,
    /// The operation is not supported by this platform, provider or model.
    Unsupported,
    Timeout,
    Cancelled,
    /// A dependency (AI server, device, ...) is not reachable right now.
    Unavailable,
    /// A resource limit (e.g. concurrent requests) was reached.
    LimitExceeded,
    Config,
    Io,
    Internal,
}

/// Serializable error returned over IPC and inside stream events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
}

impl ErrorPayload {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ErrorPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ErrorPayload {}

/// Errors produced by the JARVIS core.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Validation(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    PermissionDenied(String),

    #[error("{0}")]
    LimitExceeded(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error while {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Ai(#[from] AiError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error(transparent)]
    Memory(#[from] MemoryError),

    #[error("internal error: {0}")]
    Internal(String),
}

impl CoreError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Validation(_) => ErrorCode::Validation,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::PermissionDenied(_) => ErrorCode::PermissionDenied,
            Self::LimitExceeded(_) => ErrorCode::LimitExceeded,
            Self::Config(_) => ErrorCode::Config,
            Self::Io { .. } => ErrorCode::Io,
            Self::Internal(_) => ErrorCode::Internal,
            Self::Ai(err) => match err {
                AiError::Unsupported { .. } => ErrorCode::Unsupported,
                AiError::ModelNotFound { .. } | AiError::ProviderNotFound(_) => ErrorCode::NotFound,
                AiError::AlreadyRegistered(_) => ErrorCode::Internal,
                AiError::Unavailable(_) | AiError::Protocol(_) => ErrorCode::Unavailable,
                AiError::InvalidRequest(_) => ErrorCode::Validation,
                AiError::Timeout => ErrorCode::Timeout,
                AiError::Cancelled => ErrorCode::Cancelled,
            },
            Self::Runtime(err) => match err {
                RuntimeError::NotInstalled | RuntimeError::NotFound(_) => ErrorCode::NotFound,
                RuntimeError::InvalidInput(_) | RuntimeError::InvalidModel(_) => {
                    ErrorCode::Validation
                }
                RuntimeError::PermissionDenied(_)
                | RuntimeError::IntegrityMismatch { .. }
                | RuntimeError::BinaryChanged(_) => ErrorCode::PermissionDenied,
                RuntimeError::InsufficientMemory { .. } | RuntimeError::InsufficientDisk { .. } => {
                    ErrorCode::LimitExceeded
                }
                RuntimeError::Download(_)
                | RuntimeError::Unavailable(_)
                | RuntimeError::StartupFailed { .. } => ErrorCode::Unavailable,
                RuntimeError::StartupTimeout(_) => ErrorCode::Timeout,
                RuntimeError::Cancelled => ErrorCode::Cancelled,
                RuntimeError::Io { .. } => ErrorCode::Io,
                RuntimeError::Internal(_) => ErrorCode::Internal,
            },
            Self::Memory(err) => match err {
                MemoryError::Storage(StorageError::Io { .. }) => ErrorCode::Io,
                MemoryError::Storage(
                    StorageError::Corrupt { .. }
                    | StorageError::NewerSchema { .. }
                    | StorageError::MigrationChanged { .. }
                    | StorageError::MissingFts5,
                )
                | MemoryError::Unavailable(_)
                | MemoryError::Embedding(_) => ErrorCode::Unavailable,
                MemoryError::Storage(_) | MemoryError::Internal(_) => ErrorCode::Internal,
                MemoryError::Disabled(_) => ErrorCode::PermissionDenied,
                MemoryError::NotFound(_) => ErrorCode::NotFound,
                MemoryError::Invalid(_) => ErrorCode::Validation,
                MemoryError::MessageTooLong { .. } => ErrorCode::LimitExceeded,
            },
        }
    }

    pub fn to_payload(&self) -> ErrorPayload {
        ErrorPayload::new(self.code(), self.to_string())
    }
}

impl From<CoreError> for ErrorPayload {
    fn from(err: CoreError) -> Self {
        err.to_payload()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_errors_to_stable_codes() {
        assert_eq!(
            CoreError::Validation("x".into()).code(),
            ErrorCode::Validation
        );
        assert_eq!(CoreError::Ai(AiError::Timeout).code(), ErrorCode::Timeout);
        assert_eq!(
            CoreError::Ai(AiError::ModelNotFound { model: "m".into() }).code(),
            ErrorCode::NotFound
        );
        assert_eq!(
            CoreError::io("reading", std::io::Error::other("disk")).code(),
            ErrorCode::Io
        );
    }

    #[test]
    fn payload_serializes_code_in_camel_case() {
        let payload = CoreError::PermissionDenied("cloud disabled".into()).to_payload();
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["code"], "permissionDenied");
        assert_eq!(json["message"], "cloud disabled");
    }
}
