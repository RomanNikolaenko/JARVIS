//! IPC commands for plugins (MCP servers). Plugin folders are picked in a
//! native dialog on the Rust side, so the WebView never passes paths. A
//! plugin is saved and started only from a review the user confirmed
//! (`install_plugin` with the review's id).

use jarvis_core::jarvis_plugins::{McpServerRequest, PluginInfo, PluginReview, PluginToolUpdate};
use jarvis_core::{CoreError, ErrorPayload};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

fn payload(err: impl Into<CoreError>) -> ErrorPayload {
    err.into().to_payload()
}

#[tauri::command]
pub async fn list_plugins(state: State<'_, AppState>) -> CommandResult<Vec<PluginInfo>> {
    Ok(state.core().plugins().list())
}

/// The server's recent stderr lines.
#[tauri::command]
pub async fn get_plugin_logs(state: State<'_, AppState>, id: String) -> CommandResult<Vec<String>> {
    state.core().plugins().logs(&id).map_err(payload)
}

/// Lets the user pick a plugin folder and returns its review. `None` if
/// cancelled. Nothing is saved or started.
#[tauri::command]
pub async fn pick_plugin_folder<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<PluginReview>> {
    let picked = crate::dialogs::selection(|complete| {
        app.dialog()
            .file()
            .set_title("Choose a plugin folder (with jarvis-plugin.json)")
            .pick_folder(complete)
    })
    .await?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let folder = picked
        .into_path()
        .map_err(|err| payload(CoreError::Validation(format!("unsupported folder: {err}"))))?;
    state
        .core()
        .plugins()
        .review_folder(folder)
        .await
        .map(Some)
        .map_err(payload)
}

/// Checks an MCP server entered by the user. Nothing is saved or started.
#[tauri::command]
pub async fn review_mcp_server(
    state: State<'_, AppState>,
    request: McpServerRequest,
) -> CommandResult<PluginReview> {
    state
        .core()
        .plugins()
        .review_command(request)
        .await
        .map_err(payload)
}

/// A new review of an installed plugin (after it changed).
#[tauri::command]
pub async fn review_installed_plugin(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<PluginReview> {
    state
        .core()
        .plugins()
        .review_existing(&id)
        .await
        .map_err(payload)
}

/// Saves and starts a reviewed plugin if nothing changed since the review.
#[tauri::command]
pub async fn install_plugin(
    state: State<'_, AppState>,
    review_id: String,
) -> CommandResult<PluginInfo> {
    state
        .core()
        .plugins()
        .install(&review_id)
        .await
        .map_err(payload)
}

#[tauri::command]
pub async fn set_plugin_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> CommandResult<PluginInfo> {
    state
        .core()
        .plugins()
        .set_enabled(&id, enabled)
        .await
        .map_err(payload)
}

#[tauri::command]
pub async fn restart_plugin(state: State<'_, AppState>, id: String) -> CommandResult<PluginInfo> {
    state.core().plugins().restart(&id).await.map_err(payload)
}

#[tauri::command]
pub async fn refresh_plugin_tools(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<PluginInfo> {
    state
        .core()
        .plugins()
        .refresh_tools(&id)
        .await
        .map_err(payload)
}

#[tauri::command]
pub async fn set_plugin_tool(
    state: State<'_, AppState>,
    update: PluginToolUpdate,
) -> CommandResult<PluginInfo> {
    state
        .core()
        .plugins()
        .set_tool(update)
        .await
        .map_err(payload)
}

/// Stores a plugin secret in the OS keychain; it is never read back.
#[tauri::command]
pub async fn set_plugin_secret(
    state: State<'_, AppState>,
    id: String,
    name: String,
    value: String,
) -> CommandResult<PluginInfo> {
    state
        .core()
        .plugins()
        .set_secret(&id, &name, &value)
        .await
        .map_err(payload)
}

#[tauri::command]
pub async fn clear_plugin_secret(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> CommandResult<PluginInfo> {
    state
        .core()
        .plugins()
        .clear_secret(&id, &name)
        .await
        .map_err(payload)
}

/// Removes the plugin's settings and secrets; its folder stays.
#[tauri::command]
pub async fn remove_plugin(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state.core().plugins().remove(&id).await.map_err(payload)
}
