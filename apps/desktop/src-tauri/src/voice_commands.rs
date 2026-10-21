//! IPC commands of the voice engine. Packages are referred to by catalog
//! id, so the WebView never chooses what gets downloaded or run.

use jarvis_core::ErrorPayload;
use jarvis_core::jarvis_voice::VoiceStatus;
use tauri::State;

use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

#[tauri::command]
pub async fn get_voice_status(state: State<'_, AppState>) -> CommandResult<VoiceStatus> {
    Ok(state.core().voice_status().await)
}

/// Installs a package the user approved in the UI; returns the download id.
#[tauri::command]
pub fn install_voice_package(state: State<'_, AppState>, id: String) -> CommandResult<String> {
    state
        .core()
        .voice_install(&id)
        .map_err(|err| err.to_payload())
}

/// Deletes an installed package (the UI asks for confirmation).
#[tauri::command]
pub async fn remove_voice_package(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state
        .core()
        .voice_remove(&id)
        .await
        .map_err(|err| err.to_payload())
}

#[tauri::command]
pub fn cancel_voice_download(state: State<'_, AppState>, id: String) -> bool {
    state.core().voice_cancel_download(&id)
}

/// Opens the microphone for one spoken request (push-to-talk).
#[tauri::command]
pub fn start_listening(state: State<'_, AppState>) -> CommandResult<()> {
    state.core().voice_listen().map_err(|err| err.to_payload())
}

/// Ends the spoken request: `submit` recognizes it, otherwise it is dropped.
#[tauri::command]
pub fn stop_listening(state: State<'_, AppState>, submit: bool) {
    state.core().voice_stop_listening(submit);
}

#[tauri::command]
pub fn stop_speaking(state: State<'_, AppState>) {
    state.core().voice_stop_speaking();
}

/// Speaks a short text to try the voice settings.
#[tauri::command]
pub fn speak_text(state: State<'_, AppState>, text: String) -> CommandResult<()> {
    state
        .core()
        .voice_say(&text)
        .map_err(|err| err.to_payload())
}
