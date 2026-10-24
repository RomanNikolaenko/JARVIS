use thiserror::Error;

pub type Result<T, E = PluginError> = std::result::Result<T, E>;

/// A JSON-RPC error returned by an MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// Errors of the MCP client.
#[derive(Debug, Clone, Error)]
pub enum McpError {
    /// The server answered with an error.
    #[error("the server returned an error: {0}")]
    Rpc(RpcError),
    /// The server broke the protocol (invalid or oversized messages, wrong
    /// result shapes).
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The server needs something JARVIS does not offer.
    #[error("{0}")]
    Unsupported(String),
    #[error("the server did not answer within {0} s")]
    TimedOut(u64),
    #[error("cancelled")]
    Cancelled,
    /// The connection ended (the server exited or closed its output).
    #[error("the connection to the server ended: {0}")]
    Closed(String),
}

/// Errors of plugin management.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("invalid plugin: {0}")]
    Invalid(String),
    #[error("plugin \"{0}\" not found")]
    NotFound(String),
    /// The plugin changed since the user approved it, or was never approved.
    #[error("{0}")]
    NeedsApproval(String),
    #[error(transparent)]
    Mcp(#[from] McpError),
    #[error("cannot save plugin settings: {0}")]
    Store(String),
    #[error("keychain error: {0}")]
    Secret(String),
    #[error("I/O error while {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl PluginError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}
