//! IPC commands for captures and images attached to messages. Every capture
//! starts from a user action in the UI; files are picked in a native dialog
//! on the Rust side, so the WebView never passes paths.

use std::time::Duration;

use jarvis_core::jarvis_vision::{CaptureInfo, WindowInfo, image::MAX_FILE_BYTES};
use jarvis_core::{CoreError, ErrorPayload, VisionStatus};
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_dialog::DialogExt;

use crate::MAIN_WINDOW;
use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

/// Time for the window to disappear before the screen is captured.
const HIDE_DELAY: Duration = Duration::from_millis(300);

fn payload(err: impl Into<CoreError>) -> ErrorPayload {
    err.into().to_payload()
}

#[tauri::command]
pub async fn get_vision_status(state: State<'_, AppState>) -> CommandResult<VisionStatus> {
    Ok(state.core().vision_status().await)
}

#[tauri::command]
pub async fn list_windows(state: State<'_, AppState>) -> CommandResult<Vec<WindowInfo>> {
    state.core().list_windows().await.map_err(payload)
}

/// Captures a display; with `hideJarvis` the JARVIS window steps aside
/// while the screen is captured.
#[tauri::command]
pub async fn capture_screen<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    display: Option<String>,
    hide_jarvis: bool,
) -> CommandResult<CaptureInfo> {
    let window = hide_jarvis
        .then(|| app.get_webview_window(MAIN_WINDOW))
        .flatten();
    if let Some(window) = &window {
        let _ = window.hide();
        tokio::time::sleep(HIDE_DELAY).await;
    }
    let result = state.core().capture_screen(display.as_deref()).await;
    if let Some(window) = &window {
        let _ = window.show();
        let _ = window.set_focus();
    }
    result.map_err(payload)
}

#[tauri::command]
pub async fn capture_window(state: State<'_, AppState>, id: String) -> CommandResult<CaptureInfo> {
    state.core().capture_window(&id).await.map_err(payload)
}

/// A window or area picked in the system dialog (Wayland).
#[tauri::command]
pub async fn capture_selection(state: State<'_, AppState>) -> CommandResult<CaptureInfo> {
    state.core().capture_selection().await.map_err(payload)
}

/// Lets the user pick a PNG or JPEG file. `None` if cancelled.
#[tauri::command]
pub async fn attach_image_file<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<CaptureInfo>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Attach an image")
        .add_filter("Images", &["png", "jpg", "jpeg"])
        .blocking_pick_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|err| payload(CoreError::Validation(format!("unsupported file: {err}"))))?;
    let size = tokio::fs::metadata(&path)
        .await
        .map_err(|err| payload(CoreError::io("reading the image", err)))?
        .len();
    if size > MAX_FILE_BYTES as u64 {
        return Err(payload(CoreError::Validation(format!(
            "The image is larger than {} MB",
            MAX_FILE_BYTES / 1024 / 1024
        ))));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|err| payload(CoreError::io("reading the image", err)))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Image".into());
    state
        .core()
        .attach_image_file(&name, bytes)
        .await
        .map(Some)
        .map_err(payload)
}

/// A photo taken with the camera in the UI (a base64 `data:` URL).
#[tauri::command]
pub async fn attach_photo(state: State<'_, AppState>, data: String) -> CommandResult<CaptureInfo> {
    state.core().attach_photo(data).await.map_err(payload)
}

#[tauri::command]
pub fn discard_capture(state: State<'_, AppState>, id: String) -> bool {
    state.core().discard_capture(&id)
}
