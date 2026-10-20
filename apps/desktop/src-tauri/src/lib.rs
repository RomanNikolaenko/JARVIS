//! JARVIS desktop shell (application layer).
//!
//! Responsibilities: window and process lifecycle, resolving platform paths,
//! logging, IPC commands, and forwarding core events to the UI. Domain logic
//! lives in `jarvis-core`, which does not depend on Tauri.

mod agent_commands;
mod commands;
mod logging;
mod memory_commands;
mod runtime_commands;
mod state;

#[cfg(test)]
mod bindings;
#[cfg(test)]
mod ipc_tests;

use std::error::Error;

use jarvis_core::config::LogLevel;
use jarvis_core::jarvis_agent::AgentTask;
use jarvis_core::jarvis_security::PendingApproval;
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
    ])
}

/// Application entry point.
pub fn run() {
    let result = with_commands(tauri::Builder::default())
        .plugin(tauri_plugin_dialog::init())
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
    let paths = resolve_paths(app.handle())?;
    let logging = logging::init(&paths.log_dir, LogLevel::default())?;
    let version = app.package_info().version.to_string();
    tracing::info!(%version, log_dir = %paths.log_dir.display(), "starting JARVIS desktop");

    let core =
        tauri::async_runtime::block_on(JarvisCore::initialize(CoreOptions::new(paths, version)))?;
    logging.set_level(core.settings().logging.level);

    spawn_event_forwarder(app.handle().clone(), core.events().subscribe());
    spawn_agent_forwarders(
        app.handle().clone(),
        core.subscribe_agent_tasks(),
        core.subscribe_approvals(),
    );
    app.manage(AppState::new(core, Some(logging)));
    Ok(())
}

/// Settings roam with the user profile; data (later: databases, models) and
/// logs stay on this machine.
fn resolve_paths<R: Runtime>(app: &AppHandle<R>) -> Result<AppPaths, tauri::Error> {
    let resolver = app.path();
    Ok(AppPaths {
        config_dir: resolver.app_config_dir()?,
        data_dir: resolver.app_local_data_dir()?,
        log_dir: resolver.app_log_dir()?,
    })
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
