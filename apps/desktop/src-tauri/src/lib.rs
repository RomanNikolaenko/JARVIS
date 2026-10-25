//! JARVIS desktop shell (application layer).
//!
//! Responsibilities: window and process lifecycle, resolving platform paths,
//! logging, IPC commands, and forwarding core events to the UI. Domain logic
//! lives in `jarvis-core`, which does not depend on Tauri.

mod agent_commands;
mod commands;
mod dialogs;
mod logging;
mod memory_commands;
mod plugin_commands;
mod runtime_commands;
mod state;
mod updates;
mod vision_commands;
mod voice_commands;
mod window;

#[cfg(test)]
mod bindings;
#[cfg(test)]
mod ipc_tests;

use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use jarvis_core::config::LogLevel;
use jarvis_core::jarvis_agent::AgentTask;
use jarvis_core::jarvis_plugins::PluginManager;
use jarvis_core::jarvis_security::PendingApproval;
use jarvis_core::jarvis_voice::{VoiceEngine, VoiceEvent};
use jarvis_core::{AppPaths, CoreOptions, JarvisCore};
use jarvis_events::{EventSubscription, RecvError};
use tauri::{App, AppHandle, Builder, Emitter, Manager, RunEvent, Runtime, UserAttentionType};
use tokio::sync::{broadcast, watch};

pub use state::AppState;

/// Label of the main window; capabilities are scoped to it.
pub const MAIN_WINDOW: &str = "main";
/// Event used to push [`jarvis_events::SystemEvent`]s to the UI.
pub const SYSTEM_EVENT: &str = "jarvis://system-event";
/// Event with an [`AgentTask`] whenever a task changes.
pub const AGENT_TASK_EVENT: &str = "jarvis://agent-task";
/// Event with the list of actions waiting for approval.
pub const APPROVALS_EVENT: &str = "jarvis://approvals";
/// Event with the voice engine's state (microphone, listening, speaking).
pub const VOICE_STATE_EVENT: &str = "jarvis://voice-state";
/// Event with a recognized spoken request.
pub const VOICE_TRANSCRIPT_EVENT: &str = "jarvis://voice-transcript";
/// Event with the microphone level (0–1) while it is open.
pub const VOICE_LEVEL_EVENT: &str = "jarvis://voice-level";
/// Payload: `PluginInfo[]` after any change of a plugin.
pub const PLUGINS_EVENT: &str = "jarvis://plugins";

/// Registers all IPC commands. Shared by the application and the IPC tests.
pub fn with_commands<R: Runtime>(builder: Builder<R>) -> Builder<R> {
    builder.invoke_handler(tauri::generate_handler![
        commands::get_app_info,
        commands::get_health,
        commands::get_platform_info,
        commands::get_system_metrics,
        commands::get_settings,
        commands::update_settings,
        commands::get_recent_events,
        commands::list_ai_models,
        commands::get_model_routes,
        commands::test_ai_provider,
        commands::set_provider_api_key,
        commands::has_provider_api_key,
        commands::delete_provider_api_key,
        commands::cancel_chat,
        commands::report_ui_error,
        runtime_commands::get_runtime_status,
        runtime_commands::get_hardware_info,
        runtime_commands::list_curated_models,
        runtime_commands::check_runtime_release,
        runtime_commands::install_runtime,
        runtime_commands::select_runtime_binary,
        runtime_commands::import_model,
        runtime_commands::download_curated_model,
        runtime_commands::download_model,
        runtime_commands::cancel_download,
        runtime_commands::remove_model,
        runtime_commands::start_local_model,
        runtime_commands::stop_local_model,
        memory_commands::send_message,
        memory_commands::list_conversations,
        memory_commands::get_conversation,
        memory_commands::create_conversation,
        memory_commands::update_conversation,
        memory_commands::delete_conversation,
        memory_commands::list_facts,
        memory_commands::save_fact,
        memory_commands::delete_fact,
        memory_commands::list_projects,
        memory_commands::save_project,
        memory_commands::delete_project,
        memory_commands::list_episodes,
        memory_commands::clear_episodes,
        memory_commands::search_memory,
        memory_commands::get_memory_status,
        memory_commands::export_memory,
        memory_commands::import_memory,
        memory_commands::clear_memory,
        agent_commands::get_agent_status,
        agent_commands::list_tools,
        agent_commands::start_agent_task,
        agent_commands::list_agent_tasks,
        agent_commands::get_agent_task,
        agent_commands::cancel_agent_task,
        agent_commands::pause_agent_task,
        agent_commands::resume_agent_task,
        agent_commands::clear_finished_agent_tasks,
        agent_commands::list_pending_approvals,
        agent_commands::resolve_approval,
        agent_commands::get_audit_log,
        agent_commands::verify_audit_log,
        agent_commands::add_workspace_folder,
        voice_commands::get_voice_status,
        voice_commands::install_voice_package,
        voice_commands::remove_voice_package,
        voice_commands::cancel_voice_download,
        voice_commands::start_listening,
        voice_commands::stop_listening,
        voice_commands::stop_speaking,
        voice_commands::speak_text,
        vision_commands::get_vision_status,
        vision_commands::list_windows,
        vision_commands::capture_screen,
        vision_commands::capture_window,
        vision_commands::capture_selection,
        vision_commands::attach_image_file,
        vision_commands::attach_photo,
        vision_commands::discard_capture,
        runtime_commands::attach_projector,
        plugin_commands::list_plugins,
        plugin_commands::get_plugin_logs,
        plugin_commands::pick_plugin_folder,
        plugin_commands::review_mcp_server,
        plugin_commands::review_installed_plugin,
        plugin_commands::install_plugin,
        plugin_commands::set_plugin_enabled,
        plugin_commands::restart_plugin,
        plugin_commands::refresh_plugin_tools,
        plugin_commands::set_plugin_tool,
        plugin_commands::set_plugin_secret,
        plugin_commands::clear_plugin_secret,
        plugin_commands::remove_plugin,
        updates::get_update_status,
        updates::check_for_update,
        updates::install_update,
    ])
}

/// Application entry point.
pub fn run() {
    let result = with_commands(tauri::Builder::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(setup)
        .build(tauri::generate_context!());

    match result {
        Ok(app) => app.run(|handle, event| {
            if let RunEvent::Exit = event
                && let Some(state) = handle.try_state::<AppState>()
            {
                tauri::async_runtime::block_on(state.shutdown());
            }
        }),
        Err(err) => {
            tracing::error!("failed to start JARVIS: {err}");
            // Logging may not be initialised yet, so also report on stderr.
            #[allow(clippy::print_stderr)]
            {
                eprintln!("failed to start JARVIS: {err}");
            }
            std::process::exit(1);
        }
    }
}

fn setup<R: Runtime>(app: &mut App<R>) -> Result<(), Box<dyn Error>> {
    // First, so the window appears at once while the core starts.
    if let Some(main) = app.get_webview_window(MAIN_WINDOW) {
        window::open_main_window(&main);
    }
    let paths = resolve_paths(app.handle())?;
    let logging = logging::init(&paths.log_dir, LogLevel::default())?;
    let version = app.package_info().version.to_string();
    tracing::info!(%version, log_dir = %paths.log_dir.display(), "starting JARVIS desktop");

    let core = tauri::async_runtime::block_on(JarvisCore::initialize(CoreOptions::new(
        paths,
        version.clone(),
    )))?;
    logging.set_level(core.settings().logging.level);

    spawn_event_forwarder(app.handle().clone(), core.events().subscribe());
    spawn_agent_forwarders(
        app.handle().clone(),
        core.subscribe_agent_tasks(),
        core.subscribe_approvals(),
    );
    spawn_voice_forwarders(app.handle().clone(), core.voice());
    spawn_plugin_forwarder(app.handle().clone(), core.plugins().clone());
    let updates = updates::UpdateService::new(&version);
    updates::spawn_status_forwarder(app.handle().clone(), &updates);
    updates::spawn_automatic_checks(app.handle().clone(), Arc::clone(&updates));
    app.manage(updates);
    app.manage(AppState::new(core, Some(logging)));
    Ok(())
}

/// Keeps settings, data and logs in one folder instead of the user profile
/// (release tests, portable use). Must be an absolute path.
pub const PROFILE_DIR_ENV: &str = "JARVIS_PROFILE_DIR";

/// Settings roam with the user profile; data (databases, models) and logs
/// stay on this machine. `JARVIS_PROFILE_DIR` replaces all three.
fn resolve_paths<R: Runtime>(app: &AppHandle<R>) -> Result<AppPaths, tauri::Error> {
    // Logging starts later with these paths; the log folder is reported then.
    if let Some(profile) = profile_dir(std::env::var_os(PROFILE_DIR_ENV)) {
        return Ok(AppPaths {
            config_dir: profile.join("config"),
            data_dir: profile.join("data"),
            log_dir: profile.join("logs"),
        });
    }
    let resolver = app.path();
    Ok(AppPaths {
        config_dir: resolver.app_config_dir()?,
        data_dir: resolver.app_local_data_dir()?,
        log_dir: resolver.app_log_dir()?,
    })
}

/// The profile folder from `JARVIS_PROFILE_DIR`, if it is an absolute path.
fn profile_dir(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|dir| dir.is_absolute())
}

/// Forwards agent task changes and pending approvals to the main window.
/// A new approval request also asks the OS to draw attention to the window.
fn spawn_agent_forwarders<R: Runtime>(
    app: AppHandle<R>,
    mut tasks: broadcast::Receiver<AgentTask>,
    mut approvals: watch::Receiver<Vec<PendingApproval>>,
) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match tasks.recv().await {
                Ok(task) => {
                    if let Err(err) = handle.emit_to(MAIN_WINDOW, AGENT_TASK_EVENT, &task) {
                        tracing::debug!("cannot forward agent task: {err}");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!("UI agent forwarder skipped {skipped} task updates");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    tauri::async_runtime::spawn(async move {
        let mut count = 0;
        while approvals.changed().await.is_ok() {
            let pending = approvals.borrow_and_update().clone();
            if let Err(err) = app.emit_to(MAIN_WINDOW, APPROVALS_EVENT, &pending) {
                tracing::debug!("cannot forward approvals: {err}");
            }
            if pending.len() > count
                && let Some(window) = app.get_webview_window(MAIN_WINDOW)
                && let Err(err) =
                    window.request_user_attention(Some(UserAttentionType::Informational))
            {
                tracing::debug!("cannot request attention: {err}");
            }
            count = pending.len();
        }
    });
}

/// Forwards the voice state, recognized requests and microphone levels.
fn spawn_voice_forwarders<R: Runtime>(app: AppHandle<R>, voice: &VoiceEngine) {
    let mut events = voice.subscribe_events();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let result = match events.recv().await {
                Ok(VoiceEvent::State(state)) => {
                    handle.emit_to(MAIN_WINDOW, VOICE_STATE_EVENT, &state)
                }
                Ok(VoiceEvent::Transcript(transcript)) => {
                    handle.emit_to(MAIN_WINDOW, VOICE_TRANSCRIPT_EVENT, &transcript)
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!("UI voice forwarder skipped {skipped} events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if let Err(err) = result {
                tracing::debug!("cannot forward a voice event: {err}");
            }
        }
    });
    let mut levels = voice.subscribe_levels();
    tauri::async_runtime::spawn(async move {
        loop {
            match levels.recv().await {
                Ok(level) => {
                    let _ = app.emit_to(MAIN_WINDOW, VOICE_LEVEL_EVENT, level);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Sends the plugin list to the main window after every change.
fn spawn_plugin_forwarder<R: Runtime>(app: AppHandle<R>, plugins: PluginManager) {
    let mut changes = plugins.subscribe();
    tauri::async_runtime::spawn(async move {
        // A lagging receiver still gets the latest list.
        while let Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) = changes.recv().await {
            if let Err(err) = app.emit_to(MAIN_WINDOW, PLUGINS_EVENT, plugins.list()) {
                tracing::debug!("cannot forward plugins: {err}");
            }
        }
    });
}

/// Forwards core events to the main window only.
fn spawn_event_forwarder<R: Runtime>(app: AppHandle<R>, mut subscription: EventSubscription) {
    tauri::async_runtime::spawn(async move {
        loop {
            match subscription.recv().await {
                Ok(event) => {
                    if let Err(err) = app.emit_to(MAIN_WINDOW, SYSTEM_EVENT, &event) {
                        tracing::debug!("cannot forward system event: {err}");
                    }
                }
                Err(RecvError::Lagged { skipped }) => {
                    tracing::warn!("UI event forwarder skipped {skipped} events");
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absolute_profile_folders_are_used() {
        let absolute = std::env::temp_dir().join("jarvis-profile");
        assert_eq!(
            profile_dir(Some(absolute.clone().into_os_string())),
            Some(absolute)
        );
        assert_eq!(profile_dir(Some(OsString::from("relative/profile"))), None);
        assert_eq!(profile_dir(Some(OsString::new())), None);
        assert_eq!(profile_dir(None), None);
    }
}
