//! Windows capture through xcap (DXGI / GDI).

use async_trait::async_trait;
use xcap::{Monitor, Window};

use crate::capture::{Captured, ScreenCapture, blocking};
use crate::error::{Result, VisionError};
use crate::types::{CaptureCapabilities, CaptureSupport, DisplayInfo, WindowInfo};

#[derive(Debug)]
pub struct WindowsCapture;

fn failed(err: xcap::XCapError) -> VisionError {
    VisionError::Capture(err.to_string())
}

/// Smaller windows are helpers (tray icons, drag handles), not content.
const MIN_WINDOW_SIZE: u32 = 64;

fn monitor_name(monitor: &Monitor, index: usize) -> String {
    let friendly = monitor.friendly_name().ok();
    display_name(friendly.as_deref(), index)
}

/// The monitor's own name, unless Windows does not know it ("Unknown
/// Monitor 65537" without a driver name).
fn display_name(friendly: Option<&str>, index: usize) -> String {
    friendly
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.starts_with("Unknown Monitor"))
        .map_or_else(|| format!("Display {}", index + 1), str::to_owned)
}

/// Windows that can be captured: visible, not minimized, with a title.
fn capturable(window: &Window) -> bool {
    !window.is_minimized().unwrap_or(true)
        && window.title().is_ok_and(|title| !title.trim().is_empty())
        && window.width().unwrap_or(0) >= MIN_WINDOW_SIZE
        && window.height().unwrap_or(0) >= MIN_WINDOW_SIZE
}

#[async_trait]
impl ScreenCapture for WindowsCapture {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            screen: CaptureSupport::Available,
            window: CaptureSupport::Available,
            selection: CaptureSupport::Unavailable,
            notes: vec![
                "Windows shows no indicator of its own for screenshots; JARVIS shows every capture before it is sent.".into(),
            ],
        }
    }

    async fn displays(&self) -> Result<Vec<DisplayInfo>> {
        blocking(|| {
            let monitors = Monitor::all().map_err(failed)?;
            Ok(monitors
                .iter()
                .enumerate()
                .filter_map(|(index, monitor)| {
                    Some(DisplayInfo {
                        id: monitor.id().ok()?.to_string(),
                        name: monitor_name(monitor, index),
                        width: monitor.width().ok()?,
                        height: monitor.height().ok()?,
                        primary: monitor.is_primary().unwrap_or(false),
                    })
                })
                .collect())
        })
        .await
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>> {
        blocking(|| {
            let windows = Window::all().map_err(failed)?;
            Ok(windows
                .iter()
                .filter(|window| capturable(window))
                .filter_map(|window| {
                    Some(WindowInfo {
                        id: window.id().ok()?.to_string(),
                        title: window.title().ok()?,
                        app: window.app_name().ok().filter(|name| !name.is_empty()),
                        width: window.width().ok()?,
                        height: window.height().ok()?,
                    })
                })
                .collect())
        })
        .await
    }

    async fn capture_display(&self, id: Option<&str>) -> Result<Captured> {
        let id = id.map(str::to_owned);
        blocking(move || {
            let monitors = Monitor::all().map_err(failed)?;
            let index = match &id {
                Some(id) => monitors
                    .iter()
                    .position(|m| m.id().ok().map(|v| v.to_string()).as_ref() == Some(id))
                    .ok_or_else(|| {
                        VisionError::NotFound("the display is no longer connected".into())
                    })?,
                None => monitors
                    .iter()
                    .position(|m| m.is_primary().unwrap_or(false))
                    .unwrap_or(0),
            };
            let monitor = monitors
                .get(index)
                .ok_or_else(|| VisionError::NotFound("no display found".into()))?;
            Ok(Captured {
                image: monitor.capture_image().map_err(failed)?,
                label: monitor_name(monitor, index),
            })
        })
        .await
    }

    async fn capture_window(&self, id: &str) -> Result<Captured> {
        let id = id.to_owned();
        blocking(move || {
            let windows = Window::all().map_err(failed)?;
            let window = windows
                .iter()
                .find(|w| w.id().ok().map(|v| v.to_string()).as_ref() == Some(&id))
                .ok_or_else(|| VisionError::NotFound("the window was closed".into()))?;
            if window.is_minimized().unwrap_or(false) {
                return Err(VisionError::Capture(
                    "the window is minimized; restore it first".into(),
                ));
            }
            Ok(Captured {
                image: window.capture_image().map_err(failed)?,
                label: window.title().unwrap_or_else(|_| "Window".into()),
            })
        })
        .await
    }

    async fn capture_selection(&self) -> Result<Captured> {
        Err(VisionError::Unsupported(
            "Choose a display or a window from the list".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn unknown_monitors_get_a_number() {
        assert_eq!(display_name(Some("DELL U2720Q"), 0), "DELL U2720Q");
        assert_eq!(display_name(Some("Unknown Monitor 65537"), 0), "Display 1");
        assert_eq!(display_name(Some("  "), 1), "Display 2");
        assert_eq!(display_name(None, 2), "Display 3");
    }
}
