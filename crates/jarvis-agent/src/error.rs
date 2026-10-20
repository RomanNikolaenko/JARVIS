use jarvis_storage::StorageError;

pub type Result<T, E = AgentError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("the task does not exist")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    LimitExceeded(String),
    /// The model could not be reached or answered with an error.
    #[error("the model failed: {0}")]
    Model(String),
    #[error("internal agent error: {0}")]
    Internal(String),
}

impl AgentError {
    pub(crate) fn internal(err: impl std::fmt::Display) -> Self {
        Self::Internal(err.to_string())
    }
}
