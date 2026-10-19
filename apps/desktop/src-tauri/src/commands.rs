//! IPC commands. Each command is a thin adapter: it converts IPC input,
//! delegates to the core and maps errors to [`ErrorPayload`]. Business rules
//! live in `jarvis-core`.
//!
//! Every command must also be listed in `build.rs` and granted in
//! `capabilities/main-window.json`.

use jarvis_core::{
    AppInfo, ChatStartRequest, ChatStreamEvent, ErrorPayload, HealthReport, ProviderModels,
    Settings, SinkClosed, UiErrorReport,
};
use jarvis_events::SystemEvent;
use jarvis_platform::{PlatformInfo, SystemMetrics};
use tauri::State;
use tauri::ipc::Channel;
use uuid::Uuid;

use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

const DEFAULT_EVENT_LIMIT: u32 = 200;
const MAX_EVENT_LIMIT: u32 = 500;

#[tauri::command]
pub fn get_app_info(state: State<'_, AppState>) -> AppInfo {
    state.core().info()
}

#[tauri::command]
pub async fn get_health(state: State<'_, AppState>) -> CommandResult<HealthReport> {
    Ok(state.core().health().await)
}

#[tauri::command]
pub fn get_platform_info(state: State<'_, AppState>) -> PlatformInfo {
    state.core().platform_info().clone()
}

#[tauri::command]
pub async fn get_system_metrics(state: State<'_, AppState>) -> CommandResult<SystemMetrics> {
    Ok(state.core().system_metrics().await?)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.core().settings()
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> CommandResult<Settings> {
    let saved = state.core().update_settings(settings).await?;
    state.apply_log_level(saved.logging.level);
    Ok(saved)
}

#[tauri::command]
pub fn get_recent_events(state: State<'_, AppState>, limit: Option<u32>) -> Vec<SystemEvent> {
    let limit = limit
        .unwrap_or(DEFAULT_EVENT_LIMIT)
        .clamp(1, MAX_EVENT_LIMIT);
    state.core().events().recent(limit as usize)
}

#[tauri::command]
pub async fn list_ai_models(state: State<'_, AppState>) -> CommandResult<Vec<ProviderModels>> {
    Ok(state.core().list_models().await)
}

/// Starts a chat request and streams [`ChatStreamEvent`]s through `on_event`.
/// Returns the request id, which can be passed to [`cancel_chat`].
#[tauri::command]
pub async fn start_chat(
    state: State<'_, AppState>,
    request: ChatStartRequest,
    on_event: Channel<ChatStreamEvent>,
) -> CommandResult<Uuid> {
    let sink = move |event: ChatStreamEvent| on_event.send(event).map_err(|_| SinkClosed);
    Ok(state.core().chat().start(request, sink).await?)
}

/// Returns `false` if the request is not running (already finished or unknown).
#[tauri::command]
pub fn cancel_chat(state: State<'_, AppState>, request_id: Uuid) -> bool {
    state.core().chat().cancel(request_id)
}

#[tauri::command]
pub fn report_ui_error(state: State<'_, AppState>, report: UiErrorReport) {
    state.core().record_ui_error(report);
}
