//! JARVIS plugins: MCP servers started by JARVIS whose tools agents may
//! use through the Security Policy Engine.
//!
//! - [`mcp`]: a minimal MCP client (stdio, both protocol generations).
//! - [`process`]: starting and stopping a server process.
//! - [`tool`]: MCP tools as JARVIS tools (`<plugin>__<tool>`).
//! - [`manifest`]: the plugin manifest (`jarvis-plugin.json`).
//! - [`PluginManager`]: reviews, approvals (fingerprints), secrets in the
//!   OS keychain, lifecycle with bounded restarts, tool registration.
//!
//! A plugin runs as a separate process with the user's rights; JARVIS does
//! not sandbox it. See ADR 0012.

mod error;
mod launch;
mod manager;
pub mod manifest;
pub mod mcp;
pub mod process;
mod store;
pub mod tool;
mod types;

pub use error::{McpError, PluginError, Result, RpcError};
pub use manager::{PluginManager, PluginManagerOptions, secret_key};
pub use mcp::{ClientInfo, McpClient};
pub use types::{
    EnvVar, LaunchInfo, McpServerRequest, PluginInfo, PluginReview, PluginSecretInfo,
    PluginSourceKind, PluginStatus, PluginToolInfo, PluginToolUpdate, SecretRequirement,
    ServerDetails, ToolHints,
};
