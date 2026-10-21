//! Screen and window capture. Nothing is captured unless the user asks:
//! the core calls these only from explicit UI actions.

use std::sync::Arc;

use async_trait::async_trait;
use image::RgbaImage;

use crate::error::{Result, VisionError};
use crate::types::{CaptureCapabilities, CaptureSupport, DisplayInfo, WindowInfo};

/// A captured image and what it shows.
#[derive(Clone)]
pub struct Captured {
    pub image: RgbaImage,
    pub label: String,
}

impl std::fmt::Debug for Captured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Captured")
            .field("label", &self.label)
            .field("size", &self.image.dimensions())
            .finish()
    }
}

#[async_trait]
pub trait ScreenCapture: Send + Sync + std::fmt::Debug {
    fn capabilities(&self) -> CaptureCapabilities;
    async fn displays(&self) -> Result<Vec<DisplayInfo>>;
    async fn windows(&self) -> Result<Vec<WindowInfo>>;
    /// `None` captures the primary display.
    async fn capture_display(&self, id: Option<&str>) -> Result<Captured>;
    async fn capture_window(&self, id: &str) -> Result<Captured>;
    /// A window or area the user picks in the system dialog.
    async fn capture_selection(&self) -> Result<Captured>;
}

/// The capture backend for this system.
pub fn system_capture() -> Arc<dyn ScreenCapture> {
    #[cfg(windows)]
    {
        Arc::new(crate::windows::WindowsCapture)
    }
    #[cfg(target_os = "linux")]
    {
        use jarvis_platform::{DisplayServer, detect_linux_display_server};
        match detect_linux_display_server(|key| std::env::var(key).ok()) {
            DisplayServer::Wayland => Arc::new(crate::portal::PortalCapture),
            DisplayServer::X11 => Arc::new(crate::x11::X11Capture),
            _ => Arc::new(Unsupported::new(
                "No graphical session was found, so there is no screen to capture",
            )),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Arc::new(Unsupported::new(
            "Screen capture is available on Windows and Linux",
        ))
    }
}

/// Reports why capture is not possible here.
#[derive(Debug, Clone)]
pub struct Unsupported {
    reason: String,
}

impl Unsupported {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn error(&self) -> VisionError {
        VisionError::Unsupported(self.reason.clone())
    }
}

#[async_trait]
impl ScreenCapture for Unsupported {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            screen: CaptureSupport::Unavailable,
            window: CaptureSupport::Unavailable,
            selection: CaptureSupport::Unavailable,
            notes: vec![self.reason.clone()],
        }
    }

    async fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Err(self.error())
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>> {
        Err(self.error())
    }

    async fn capture_display(&self, _id: Option<&str>) -> Result<Captured> {
        Err(self.error())
    }

    async fn capture_window(&self, _id: &str) -> Result<Captured> {
        Err(self.error())
    }

    async fn capture_selection(&self) -> Result<Captured> {
        Err(self.error())
    }
}

/// Runs blocking capture code off the async runtime.
pub(crate) async fn blocking<T, F>(task: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|err| VisionError::Capture(err.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsupported_systems_say_why() {
        let capture = Unsupported::new("no screen here");
        assert_eq!(capture.capabilities().screen, CaptureSupport::Unavailable);
        assert_eq!(capture.capabilities().notes, vec!["no screen here"]);
        let err = capture
            .capture_display(None)
            .await
            .expect_err("unsupported");
        assert!(matches!(err, VisionError::Unsupported(ref r) if r == "no screen here"));
    }
}
