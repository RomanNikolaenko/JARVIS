//! IPC commands of mentor mode. Writes are never started from here: they
//! come from the mentor's reply and go through the security policy and the
//! approval dialog in the core.

use jarvis_core::{ErrorPayload, MentorCommand, MentorStartRequest, MentorStatus};
use tauri::State;

use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

/// Starts a lesson: the project folder must be in a workspace folder.
#[tauri::command]
pub async fn mentor_start(
    state: State<'_, AppState>,
    request: MentorStartRequest,
) -> CommandResult<MentorStatus> {
    state
        .core()
        .mentor_start(request)
        .await
        .map_err(|err| err.to_payload())
}

#[tauri::command]
pub fn mentor_stop(state: State<'_, AppState>) {
    state.core().mentor_stop();
}

#[tauri::command]
pub fn mentor_pause(state: State<'_, AppState>) {
    state.core().mentor_pause();
}

#[tauri::command]
pub fn mentor_resume(state: State<'_, AppState>) -> CommandResult<MentorStatus> {
    state.core().mentor_resume().map_err(|err| err.to_payload())
}

#[tauri::command]
pub fn mentor_status(state: State<'_, AppState>) -> MentorStatus {
    state.core().mentor_status()
}

#[tauri::command]
pub fn mentor_stop_typing(state: State<'_, AppState>) {
    state.core().mentor_stop_typing();
}

/// The UI is answering, speaking or holding a phrase: JARVIS does not speak
/// up about saved files meanwhile.
#[tauri::command]
pub fn mentor_report_busy(state: State<'_, AppState>, busy: bool) {
    state.core().mentor_set_busy(busy);
}

#[tauri::command]
pub fn mentor_owner_spoke(state: State<'_, AppState>) {
    state.core().mentor_owner_spoke();
}

/// The command a recognized phrase is, if the whole phrase is one.
#[tauri::command]
pub fn mentor_command(text: String) -> Option<MentorCommand> {
    jarvis_core::parse_command(&text)
}
