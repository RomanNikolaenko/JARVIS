//! Plugin types shared with the UI (exported to TypeScript).

use jarvis_security::RiskLevel;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a server says about a tool. Untrusted: shown as hints, never used
/// for decisions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct ToolHints {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub enum PluginSourceKind {
    /// A folder with a `jarvis-plugin.json` manifest.
    Folder,
    /// An MCP server entered as a command.
    Command,
}

/// A secret a plugin needs (passed as an environment variable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "plugins.ts")]
pub struct SecretRequirement {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// How a plugin's server is started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct LaunchInfo {
    /// As written in the manifest or by the user.
    pub program: String,
    /// The file that runs; `None` when it cannot be found.
    pub resolved_program: Option<String>,
    pub args: Vec<String>,
    pub cwd: String,
    /// Names of the plugin's own environment variables (values stay in the
    /// backend), secrets not included.
    pub env_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub enum PluginStatus {
    Disabled,
    /// Never approved, or changed since the approval.
    NeedsApproval {
        reason: String,
    },
    /// Secrets must be entered before the server can start.
    NeedsSecrets {
        names: Vec<String>,
    },
    Starting,
    Running,
    #[serde(rename_all = "camelCase")]
    Failed {
        error: String,
        /// When the next automatic start is due, if any.
        retry_in_secs: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct ServerDetails {
    pub name: String,
    pub version: String,
    pub title: Option<String>,
    pub protocol_version: String,
    /// Instructions the server offers models (not used by JARVIS).
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct PluginToolInfo {
    /// The server's name of the tool.
    pub name: String,
    /// The name agents use (`<plugin>__<tool>`).
    pub id: String,
    pub title: String,
    pub description: String,
    pub enabled: bool,
    /// Risk used for calls (high unless the user lowered it).
    pub risk: RiskLevel,
    /// The user lowered the risk, but the tool changed since, so it is high
    /// again.
    pub risk_reset: bool,
    pub hints: ToolHints,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct PluginSecretInfo {
    pub name: String,
    pub description: Option<String>,
    /// A value is stored in the OS keychain.
    pub set: bool,
}

/// A plugin as the Plugins screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub source: PluginSourceKind,
    pub folder: Option<String>,
    pub launch: LaunchInfo,
    pub enabled: bool,
    pub status: PluginStatus,
    pub server: Option<ServerDetails>,
    pub tools: Vec<PluginToolInfo>,
    /// Tools the server listed that cannot be used, and why.
    pub tool_problems: Vec<String>,
    pub secrets: Vec<PluginSecretInfo>,
    pub warnings: Vec<String>,
    /// The fingerprint the user approved.
    pub fingerprint: Option<String>,
    #[ts(type = "number")]
    pub added_at_ms: u64,
}

/// What the user sees before a plugin is added or approved again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct PluginReview {
    /// Identifies this review for [`crate::PluginManager::install`].
    pub review_id: String,
    pub source: PluginSourceKind,
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub folder: Option<String>,
    pub launch: LaunchInfo,
    pub secrets: Vec<SecretRequirement>,
    /// SHA-256 of the launch settings and the files that run.
    pub fingerprint: String,
    pub warnings: Vec<String>,
    /// A plugin with this id exists and will be replaced (its secrets and
    /// tool choices are kept).
    pub replaces: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

/// An MCP server entered by the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct McpServerRequest {
    pub name: String,
    /// A program name found in `PATH`, or an absolute path.
    pub program: String,
    pub args: Vec<String>,
    /// Working folder (absolute); JARVIS chooses one when unset.
    pub cwd: Option<String>,
    pub env: Vec<EnvVar>,
    /// Environment variables whose values are secrets (entered later and
    /// kept in the OS keychain).
    pub secret_names: Vec<String>,
}

/// The user's choice for one tool of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "plugins.ts")]
pub struct PluginToolUpdate {
    pub plugin_id: String,
    /// The server's name of the tool.
    pub tool: String,
    pub enabled: bool,
    pub risk: RiskLevel,
}
