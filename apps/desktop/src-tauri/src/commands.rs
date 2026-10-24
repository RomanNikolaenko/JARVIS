//! IPC commands. Each command is a thin adapter: it converts IPC input,
//! delegates to the core and maps errors to [`ErrorPayload`]. Business rules
//! live in `jarvis-core`.
//!
//! Every command must also be listed in `build.rs` and granted in
//! `capabilities/main-window.json`.

use jarvis_ai::ProviderConfig;
use jarvis_core::{
    AppInfo, ErrorPayload, HealthReport, ProviderModels, ProviderTestResult, RouteResolution,
    Settings, UiErrorReport,
};
use jarvis_events::SystemEvent;
use jarvis_platform::{PlatformInfo, SystemMetrics};
use tauri::State;
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

/// Lists models of all providers. Results are cached briefly; pass
/// `refresh: true` to query the servers again.
#[tauri::command]
pub async fn list_ai_models(
    state: State<'_, AppState>,
    refresh: Option<bool>,
) -> CommandResult<Vec<ProviderModels>> {
    Ok(state.core().list_models(refresh.unwrap_or(false)).await)
}

#[tauri::command]
pub async fn get_model_routes(state: State<'_, AppState>) -> CommandResult<Vec<RouteResolution>> {
    Ok(state.core().resolve_routes().await)
}

/// Connects to an unsaved provider configuration and lists its models.
#[tauri::command]
pub async fn test_ai_provider(
    state: State<'_, AppState>,
    config: ProviderConfig,
    api_key: Option<String>,
) -> CommandResult<ProviderTestResult> {
    Ok(state.core().test_provider_with_key(config, api_key).await?)
}

#[tauri::command]
pub async fn set_provider_api_key(
    state: State<'_, AppState>,
    provider_id: String,
    api_key: String,
) -> CommandResult<()> {
    Ok(state
        .core()
        .set_provider_api_key(&provider_id, &api_key)
        .await?)
}

#[tauri::command]
pub async fn has_provider_api_key(
    state: State<'_, AppState>,
    provider_id: String,
) -> CommandResult<bool> {
    Ok(state.core().has_provider_api_key(&provider_id).await?)
}

#[tauri::command]
pub async fn delete_provider_api_key(
    state: State<'_, AppState>,
    provider_id: String,
) -> CommandResult<()> {
    Ok(state.core().delete_provider_api_key(&provider_id).await?)
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
