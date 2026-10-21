//! JARVIS vision: screen and window capture, and images for vision models.
//!
//! - [`capture`]: displays and windows on Windows (xcap), X11 (x11rb) and
//!   Wayland (the xdg-desktop-portal Screenshot dialog). Capture happens
//!   only when the user asks; on systems without capture the backend
//!   reports why instead of pretending to work.
//! - [`image`]: decoding with limits, downscaling and JPEG encoding for
//!   models, and small previews for the UI.

pub mod capture;
pub mod error;
pub mod image;
#[cfg(target_os = "linux")]
mod portal;
pub mod types;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

pub use capture::{Captured, ScreenCapture, Unsupported, system_capture};
pub use error::{Result, VisionError};
pub use types::{
    CaptureCapabilities, CaptureInfo, CaptureSupport, DisplayInfo, ImageSource, WindowInfo,
};
