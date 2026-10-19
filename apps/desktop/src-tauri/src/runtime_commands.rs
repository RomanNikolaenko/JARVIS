//! IPC commands of the embedded llama.cpp runtime.
//!
//! File pickers are opened on the Rust side: the WebView never receives file
//! system access or arbitrary paths to pass back, and no dialog permission is
//! granted to it.

use jarvis_core::{CoreError, ErrorPayload};
use jarvis_platform::HardwareInfo;
use jarvis_runtime::{
    CuratedModel, LocalModel, ModelDownloadRequest, RuntimeBackend, RuntimeBinary, RuntimeManager,
    RuntimeRelease, RuntimeStatus,
};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

fn payload(err: impl Into<CoreError>) -> ErrorPayload {
    err.into().to_payload()
}

#[tauri::command]
pub async fn get_runtime_status(state: State<'_, AppState>) -> CommandResult<RuntimeStatus> {
    Ok(state.core().runtime().status().await)
}

#[tauri::command]
pub async fn get_hardware_info() -> CommandResult<HardwareInfo> {
    Ok(RuntimeManager::hardware().await)
}

#[tauri::command]
pub fn list_curated_models() -> Vec<CuratedModel> {
    RuntimeManager::curated_models()
}

/// Looks up the newest official llama.cpp build (network: GitHub API).
#[tauri::command]
pub async fn check_runtime_release(
    state: State<'_, AppState>,
    backend: RuntimeBackend,
) -> CommandResult<RuntimeRelease> {
    state
        .core()
        .runtime()
        .check_release(backend)
        .await
        .map_err(payload)
}

/// Installs a release the user approved in the UI. The core re-verifies it
/// against GitHub before downloading. Returns the download id.
#[tauri::command]
pub async fn install_runtime(
    state: State<'_, AppState>,
    release: RuntimeRelease,
) -> CommandResult<String> {
    state
        .core()
        .runtime()
        .install(release)
        .await
        .map_err(payload)
}

/// Lets the user pick an existing `llama-server`. `None` if cancelled.
#[tauri::command]
pub async fn select_runtime_binary<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<RuntimeBinary>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Select llama-server")
        .blocking_pick_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|err| payload(CoreError::Validation(format!("unsupported file: {err}"))))?;
    state
        .core()
        .runtime()
        .select_binary(&path)
        .await
        .map(Some)
        .map_err(payload)
}

/// Lets the user pick a GGUF file to import in place. `None` if cancelled.
#[tauri::command]
pub async fn import_model<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<LocalModel>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Import a GGUF model")
        .add_filter("GGUF model", &["gguf"])
        .blocking_pick_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|err| payload(CoreError::Validation(format!("unsupported file: {err}"))))?;
    state
        .core()
        .runtime()
        .import_model(&path)
        .await
        .map(Some)
        .map_err(payload)
}

#[tauri::command]
pub async fn download_curated_model(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<String> {
    state
        .core()
        .runtime()
        .download_curated(&id)
        .await
        .map_err(payload)
}

#[tauri::command]
pub async fn download_model(
    state: State<'_, AppState>,
    request: ModelDownloadRequest,
) -> CommandResult<String> {
    state
        .core()
        .runtime()
        .download_model(request)
        .await
        .map_err(payload)
}

#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>, id: String) -> bool {
    state.core().runtime().cancel_download(&id)
}

#[tauri::command]
pub async fn remove_model(
    state: State<'_, AppState>,
    id: String,
    delete_file: bool,
) -> CommandResult<()> {
    state
        .core()
        .runtime()
        .remove_model(&id, delete_file)
        .await
        .map_err(payload)
}

/// Starts serving a model; readiness is reported through the status.
#[tauri::command]
pub async fn start_local_model(state: State<'_, AppState>, model_id: String) -> CommandResult<()> {
    state
        .core()
        .start_local_model(&model_id)
        .await
        .map_err(ErrorPayload::from)
}

#[tauri::command]
pub async fn stop_local_model(state: State<'_, AppState>) -> CommandResult<()> {
    state.core().runtime().stop().await;
    Ok(())
}
