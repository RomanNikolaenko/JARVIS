//! Application updates as the UI sees them. The updater itself runs in the
//! desktop shell (it is part of Tauri); the core only defines the shared
//! types and the setting (ADR 0014).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum UpdatePhase {
    /// No check yet.
    Idle,
    Checking,
    UpToDate,
    /// A newer version was found; nothing is downloaded yet.
    Available,
    Downloading,
    /// The signature was verified; JARVIS is stopping and installing.
    Installing,
    Failed,
}

/// A newer version announced by the update feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct AvailableUpdate {
    pub version: String,
    /// Release notes from the feed (untrusted text, shown as plain text).
    pub notes: Option<String>,
    /// RFC 3339 publication date.
    pub date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct UpdateStatus {
    pub current_version: String,
    pub phase: UpdatePhase,
    pub available: Option<AvailableUpdate>,
    #[ts(type = "number | null")]
    pub downloaded_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub last_checked_ms: Option<u64>,
    pub error: Option<String>,
}

impl UpdateStatus {
    pub fn new(current_version: impl Into<String>) -> Self {
        Self {
            current_version: current_version.into(),
            phase: UpdatePhase::Idle,
            available: None,
            downloaded_bytes: None,
            total_bytes: None,
            last_checked_ms: None,
            error: None,
        }
    }
}
