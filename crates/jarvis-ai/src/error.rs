use crate::ProviderId;

/// Errors produced by AI providers and the provider registry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AiError {
    /// The provider does not implement the requested operation (for example
    /// embeddings on a chat-only backend).
    #[error("provider `{provider}` does not support `{operation}`")]
    Unsupported {
        provider: ProviderId,
        operation: &'static str,
    },

    #[error("model `{model}` is not available")]
    ModelNotFound { model: String },

    #[error("AI provider `{0}` is not registered")]
    ProviderNotFound(ProviderId),

    #[error("AI provider `{0}` is already registered")]
    AlreadyRegistered(ProviderId),

    /// The backend cannot be reached or is not ready (connection refused,
    /// model still loading, ...). Usually transient.
    #[error("AI provider is unavailable: {0}")]
    Unavailable(String),

    #[error("invalid AI request: {0}")]
    InvalidRequest(String),

    #[error("AI request timed out")]
    Timeout,

    #[error("AI request was cancelled")]
    Cancelled,

    /// The backend answered with something that violates its protocol.
    #[error("AI provider protocol error: {0}")]
    Protocol(String),

    /// The model declined to answer (a provider's safety policy).
    #[error("the model declined the request: {0}")]
    Refused(String),

    /// A usage limit of the account or subscription is reached; retrying
    /// before it resets does not help.
    #[error("usage limit reached: {0}")]
    LimitReached(String),
}

impl AiError {
    /// Whether retrying the *same idempotent* operation may succeed.
    ///
    /// Callers must still never retry non-idempotent actions automatically
    /// (see ADR 0003).
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::Timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_connectivity_errors_are_transient() {
        assert!(AiError::Timeout.is_transient());
        assert!(AiError::Unavailable("connection refused".into()).is_transient());
        assert!(!AiError::Cancelled.is_transient());
        assert!(!AiError::InvalidRequest("empty".into()).is_transient());
        assert!(!AiError::ModelNotFound { model: "x".into() }.is_transient());
        assert!(!AiError::Refused("cyber".into()).is_transient());
        assert!(!AiError::LimitReached("try again at 4 PM".into()).is_transient());
    }
}
