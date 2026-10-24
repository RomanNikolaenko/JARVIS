//! IPC commands of the memory: conversations and chat turns, facts,
//! projects, the action history, search, export and import.
//!
//! Export and import files are chosen in native dialogs opened on the Rust
//! side; the WebView never passes file paths.

use jarvis_core::jarvis_memory::{
    Conversation, ConversationDetail, ConversationPatch, Episode, EpisodeCursor, Fact, FactInput,
    MemoryStatus, Project, ProjectInput, SearchHit, SearchQuery, TransferReport, text,
};
use jarvis_core::{ChatStreamEvent, ChatTurnRequest, CoreError, ErrorPayload, SinkClosed};
use tauri::ipc::Channel;
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use crate::state::AppState;

type CommandResult<T> = Result<T, ErrorPayload>;

/// Phrase the UI sends after the user confirmed deleting all memory.
pub const CLEAR_CONFIRMATION: &str = "DELETE ALL MEMORY";

fn payload(err: impl Into<CoreError>) -> ErrorPayload {
    err.into().to_payload()
}

/// Sends a message in a conversation and streams the reply through
/// `on_event`. Returns the request id for [`crate::commands::cancel_chat`].
#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    request: ChatTurnRequest,
    on_event: Channel<ChatStreamEvent>,
) -> CommandResult<Uuid> {
    let sink = move |event: ChatStreamEvent| on_event.send(event).map_err(|_| SinkClosed);
    Ok(state.core().send_message(request, sink).await?)
}

#[tauri::command]
pub async fn list_conversations(
    state: State<'_, AppState>,
    query: Option<String>,
    limit: Option<u32>,
) -> CommandResult<Vec<Conversation>> {
    state
        .core()
        .memory()
        .list_conversations(query.as_deref(), limit.unwrap_or(200))
        .await
        .map_err(payload)
}

#[tauri::command]
pub async fn get_conversation(
    state: State<'_, AppState>,
    id: Uuid,
) -> CommandResult<ConversationDetail> {
    state
        .core()
        .memory()
        .conversation(id)
        .await
        .map_err(payload)
}

#[tauri::command]
pub async fn create_conversation(
    state: State<'_, AppState>,
    project_id: Option<Uuid>,
    temporary: bool,
) -> CommandResult<Conversation> {
    Ok(state
        .core()
        .create_conversation(project_id, temporary)
        .await?)
}

#[tauri::command]
pub async fn update_conversation(
    state: State<'_, AppState>,
    id: Uuid,
    patch: ConversationPatch,
) -> CommandResult<Conversation> {
    Ok(state.core().update_conversation(id, patch).await?)
}

#[tauri::command]
pub async fn delete_conversation(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.core().delete_conversation(id).await?)
}

#[tauri::command]
pub async fn list_facts(state: State<'_, AppState>) -> CommandResult<Vec<Fact>> {
    state.core().memory().list_facts().await.map_err(payload)
}

#[tauri::command]
pub async fn save_fact(state: State<'_, AppState>, input: FactInput) -> CommandResult<Fact> {
    Ok(state.core().save_fact(input).await?)
}

#[tauri::command]
pub async fn delete_fact(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.core().delete_fact(id).await?)
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> CommandResult<Vec<Project>> {
    state.core().memory().list_projects().await.map_err(payload)
}

#[tauri::command]
pub async fn save_project(
    state: State<'_, AppState>,
    input: ProjectInput,
) -> CommandResult<Project> {
    Ok(state.core().save_project(input).await?)
}

#[tauri::command]
pub async fn delete_project(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.core().delete_project(id).await?)
}

#[tauri::command]
pub async fn list_episodes(
    state: State<'_, AppState>,
    limit: Option<u32>,
    before: Option<EpisodeCursor>,
) -> CommandResult<Vec<Episode>> {
    state
        .core()
        .memory()
        .list_episodes(limit.unwrap_or(100), before)
        .await
        .map_err(payload)
}

/// Deletes the whole action history; returns how many entries were removed.
#[tauri::command]
pub async fn clear_episodes(state: State<'_, AppState>) -> CommandResult<u32> {
    let removed = state.core().clear_episodes().await?;
    Ok(u32::try_from(removed).unwrap_or(u32::MAX))
}

#[tauri::command]
pub async fn search_memory(
    state: State<'_, AppState>,
    query: SearchQuery,
) -> CommandResult<Vec<SearchHit>> {
    Ok(state.core().search_memory(query).await?)
}

#[tauri::command]
pub async fn get_memory_status(state: State<'_, AppState>) -> CommandResult<MemoryStatus> {
    Ok(state.core().memory_status().await)
}

/// Asks where to save and writes all saved memory there. `None` when the
/// user cancelled the dialog.
#[tauri::command]
pub async fn export_memory<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<TransferReport>> {
    let today = text::format_date(i64::try_from(unix_millis()).unwrap_or(i64::MAX));
    let picked = crate::dialogs::selection(|complete| {
        app.dialog()
            .file()
            .set_title("Export JARVIS memory")
            .set_file_name(format!("jarvis-memory-{today}.json"))
            .add_filter("JSON", &["json"])
            .save_file(complete)
    })
    .await?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|err| payload(CoreError::Validation(format!("unsupported file: {err}"))))?;
    Ok(Some(state.core().export_memory(&path).await?))
}

/// Asks for a memory export and adds its content. `None` when the user
/// cancelled the dialog.
#[tauri::command]
pub async fn import_memory<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> CommandResult<Option<TransferReport>> {
    let picked = crate::dialogs::selection(|complete| {
        app.dialog()
            .file()
            .set_title("Import JARVIS memory")
            .add_filter("JARVIS memory export", &["json"])
            .pick_file(complete)
    })
    .await?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|err| payload(CoreError::Validation(format!("unsupported file: {err}"))))?;
    Ok(Some(state.core().import_memory(&path).await?))
}

/// Deletes all saved memory. Requires [`CLEAR_CONFIRMATION`], sent by the UI
/// only after the user confirmed.
#[tauri::command]
pub async fn clear_memory(state: State<'_, AppState>, confirmation: String) -> CommandResult<()> {
    if confirmation != CLEAR_CONFIRMATION {
        return Err(payload(CoreError::Validation(
            "Deleting all memory needs the confirmation phrase".to_owned(),
        )));
    }
    Ok(state.core().clear_memory().await?)
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
