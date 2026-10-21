//! Wayland capture through the xdg-desktop-portal Screenshot interface.
//! The desktop shows its own dialog: the user confirms, or picks a window
//! or area, before anything is captured.

use ashpd::desktop::ResponseError;
use ashpd::desktop::screenshot::Screenshot;
use async_trait::async_trait;

use crate::capture::{Captured, ScreenCapture};
use crate::error::{Result, VisionError};
use crate::types::{CaptureCapabilities, CaptureSupport, DisplayInfo, WindowInfo};

#[derive(Debug)]
pub struct PortalCapture;

fn portal_error(err: ashpd::Error) -> VisionError {
    match err {
        ashpd::Error::Response(ResponseError::Cancelled) => VisionError::Cancelled,
        ashpd::Error::Zbus(_) | ashpd::Error::PortalNotFound(_) => VisionError::Unsupported(
            "The desktop has no screenshot portal (xdg-desktop-portal) running".into(),
        ),
        other => VisionError::Capture(other.to_string()),
    }
}

/// `file:///path` (percent-encoded) to a path.
fn file_path(uri: &str) -> Result<String> {
    let encoded = uri
        .strip_prefix("file://")
        .ok_or_else(|| VisionError::Capture(format!("unexpected screenshot location {uri}")))?;
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = encoded.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| VisionError::Capture("the screenshot path is not UTF-8".into()))
}

async fn take(interactive: bool) -> Result<Captured> {
    let request = Screenshot::request()
        .interactive(interactive)
        .modal(true)
        .send()
        .await
        .map_err(portal_error)?;
    let screenshot = request.response().map_err(portal_error)?;
    let path = file_path(screenshot.uri().as_str())?;
    // The file belongs to the desktop (often in the Pictures folder); it
    // is read but not deleted.
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|err| VisionError::io("reading the screenshot", err))?;
    let image = crate::image::decode(&bytes)?;
    Ok(Captured {
        image,
        label: if interactive {
            "Selected window or area".into()
        } else {
            "Screen".into()
        },
    })
}

#[async_trait]
impl ScreenCapture for PortalCapture {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            screen: CaptureSupport::SystemDialog,
            window: CaptureSupport::Unavailable,
            selection: CaptureSupport::SystemDialog,
            notes: vec![
                "On Wayland the desktop asks for permission in its own dialog; windows are picked there too.".into(),
                "The desktop may keep a copy of each screenshot in your Pictures folder.".into(),
            ],
        }
    }

    async fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(Vec::new())
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>> {
        Err(VisionError::Unsupported(
            "On Wayland, pick the window in the system dialog".into(),
        ))
    }

    async fn capture_display(&self, _id: Option<&str>) -> Result<Captured> {
        take(false).await
    }

    async fn capture_window(&self, _id: &str) -> Result<Captured> {
        Err(VisionError::Unsupported(
            "On Wayland, pick the window in the system dialog".into(),
        ))
    }

    async fn capture_selection(&self) -> Result<Captured> {
        take(true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_uris_become_paths() {
        assert_eq!(
            file_path("file:///home/user/Pictures/Screenshot%20from%202026.png").expect("path"),
            "/home/user/Pictures/Screenshot from 2026.png"
        );
        assert_eq!(
            file_path("file:///tmp/%D0%97%D0%BD%D1%96%D0%BC%D0%BE%D0%BA.png").expect("path"),
            "/tmp/Знімок.png"
        );
        assert!(file_path("https://example.com/x.png").is_err());
    }
}
