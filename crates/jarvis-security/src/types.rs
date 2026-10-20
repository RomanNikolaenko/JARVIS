//! Security types shared with the UI (exported to TypeScript).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a tool call may do. Tools declare the capabilities they need; the
/// policy decides per call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub enum Capability {
    /// Read files and directories inside the workspace.
    FsRead,
    /// Create or change files inside the workspace.
    FsWrite,
    /// Delete files inside the workspace.
    FsDelete,
    /// Start programs.
    ProcessSpawn,
    /// Stop programs JARVIS started.
    ProcessControl,
    /// Read system information (CPU, memory, processes).
    SystemRead,
    /// Fetch web pages.
    NetFetch,
    /// Open a URL in the user's browser.
    OpenExternal,
}

/// How much harm a call could do if it is wrong or was induced by injected
/// content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub enum RiskLevel {
    /// Reads, no side effects outside JARVIS.
    Low,
    /// Changes inside the workspace or sends data out.
    Medium,
    /// Hard to undo: deletes, runs programs.
    High,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A concrete action a tool is about to perform, described before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRequest {
    pub tool: String,
    pub capabilities: Vec<Capability>,
    pub risk: RiskLevel,
    /// One line for people, e.g. `Run git status in D:\work\app`.
    pub summary: String,
    /// Files or directories the call touches (already absolute).
    pub paths: Vec<PathBuf>,
    /// Program the call starts.
    pub program: Option<String>,
    /// URL the call contacts.
    pub url: Option<String>,
}

/// Result of a policy check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export_to = "security.ts")]
pub enum PolicyDecision {
    Allow,
    RequireApproval { reason: String },
    Deny { reason: String },
}
