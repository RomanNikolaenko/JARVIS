//! Capture capabilities and results shown in the UI.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How a kind of capture works on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "vision.ts")]
pub enum CaptureSupport {
    /// JARVIS captures directly when the user asks.
    Available,
    /// The system shows its own dialog (Wayland portal) where the user
    /// confirms or picks what to share.
    SystemDialog,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "vision.ts")]
pub struct CaptureCapabilities {
    /// Whole displays.
    pub screen: CaptureSupport,
    /// A window chosen from [`WindowInfo`]s.
    pub window: CaptureSupport,
    /// A window or area picked in the system dialog.
    pub selection: CaptureSupport,
    /// How capture works here, or why it does not.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "vision.ts")]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "vision.ts")]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    /// Application name, when the system reports it.
    pub app: Option<String>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "vision.ts")]
pub enum ImageSource {
    Screen,
    Window,
    /// Picked in the system dialog.
    Selection,
    File,
    Camera,
}

/// An image ready to be attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "vision.ts")]
pub struct CaptureInfo {
    pub id: String,
    pub source: ImageSource,
    /// What was captured ("Display 1", a window title, a file name).
    pub label: String,
    /// Size sent to the model (after downscaling).
    pub width: u32,
    pub height: u32,
    /// A small JPEG `data:` URL for the preview.
    pub preview: String,
    #[ts(type = "number")]
    pub captured_at_ms: u64,
}
