//! IPC commands of agents and tools: tasks, approvals and the audit log.
//!
//! Workspace folders are added through a native folder dialog opened on the
//! Rust side; the user picks them, not the model.

use jarvis_core::jarvis_agent::{AgentTask, TaskSummary};
use jarvis_core::jarvis_security::{AuditEntry, AuditVerification, PendingApproval};
use jarvis_core::{AgentStartRequest, AgentStatus, CoreError, ErrorPayload, Settings, ToolInfo};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

#[tauri::command]
pub fn get_agent_status(state: State<'_, AppState>) -> AgentStatus {
    state.core().agent_status()
}

#[tauri::command]
pub fn list_tools(state: State<'_, AppState>) -> Vec<ToolInfo> {
    state.core().list_tools()
}

#[tauri::command]
pub async fn start_agent_task(
    state: State<'_, AppState>,
    request: AgentStartRequest,
) -> CommandResult<AgentTask> {
    Ok(state.core().start_agent_task(request).await?)
}

#[tauri::command]
pub async fn list_agent_tasks(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> CommandResult<Vec<TaskSummary>> {
    Ok(state.core().list_agent_tasks(limit.unwrap_or(100)).await?)
}

#[tauri::command]
pub async fn get_agent_task(state: State<'_, AppState>, id: Uuid) -> CommandResult<AgentTask> {
    Ok(state.core().agent_task(id).await?)
}

/// `false` if the task had already finished.
#[tauri::command]
pub async fn cancel_agent_task(state: State<'_, AppState>, id: Uuid) -> CommandResult<bool> {
    Ok(state.core().cancel_agent_task(id).await?)
}

/// `false` if the task is not running.
#[tauri::command]
pub fn pause_agent_task(state: State<'_, AppState>, id: Uuid) -> bool {
    state.core().pause_agent_task(id)
}

#[tauri::command]
pub async fn resume_agent_task(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.core().resume_agent_task(id).await?)
}

/// Deletes finished tasks; returns how many.
#[tauri::command]
pub async fn clear_finished_agent_tasks(state: State<'_, AppState>) -> CommandResult<u32> {
    let removed = state.core().clear_finished_agent_tasks().await?;
    Ok(u32::try_from(removed).unwrap_or(u32::MAX))
}

#[tauri::command]
pub fn list_pending_approvals(state: State<'_, AppState>) -> Vec<PendingApproval> {
    state.core().pending_approvals()
}

/// The user's decision on a held action. `false` if it is no longer
/// pending (decided, timed out or cancelled).
#[tauri::command]
pub fn resolve_approval(state: State<'_, AppState>, id: Uuid, approved: bool) -> bool {
    state.core().resolve_approval(id, approved)
}

#[tauri::command]
pub async fn get_audit_log(
    state: State<'_, AppState>,
    limit: Option<u32>,
    before_id: Option<Uuid>,
) -> CommandResult<Vec<AuditEntry>> {
    Ok(state
        .core()
        .audit_log(limit.unwrap_or(100), before_id)
        .await?)
}

#[tauri::command]
pub async fn verify_audit_log(state: State<'_, AppState>) -> CommandResult<AuditVerification> {
    Ok(state.core().verify_audit_log().await?)
}

/// Asks for a folder and adds it to the workspace folders. `None` when the
/// user cancelled the dialog.
#[tauri::command]
pub async fn add_workspace_folder<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<Settings>> {
    let picked = crate::dialogs::selection(|complete| {
        app.dialog()
            .file()
            .set_title("Add a workspace folder for JARVIS tools")
            .pick_folder(complete)
    })
    .await?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|err| {
        ErrorPayload::from(CoreError::Validation(format!("unsupported folder: {err}")))
    })?;
    let folder = path.display().to_string();
    let mut settings = state.core().settings();
    if !settings.tools.workspace_folders.contains(&folder) {
        settings.tools.workspace_folders.push(folder);
    }
    Ok(Some(state.core().update_settings(settings).await?))
}
