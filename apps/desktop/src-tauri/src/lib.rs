//! JARVIS desktop shell (application layer).
//!
//! Responsibilities: window and process lifecycle, resolving platform paths,
//! logging, IPC commands, and forwarding core events to the UI. Domain logic
//! lives in `jarvis-core`, which does not depend on Tauri.

mod commands;
mod logging;
mod state;

#[cfg(test)]
mod bindings;
#[cfg(test)]
mod ipc_tests;

use std::error::Error;

use jarvis_core::config::LogLevel;
use jarvis_core::{AppPaths, CoreOptions, JarvisCore};
use jarvis_events::{EventSubscription, RecvError};
use tauri::{App, AppHandle, Builder, Emitter, Manager, RunEvent, Runtime};

pub use state::AppState;

/// Label of the main window; capabilities are scoped to it.
pub const MAIN_WINDOW: &str = "main";
/// Event used to push [`jarvis_events::SystemEvent`]s to the UI.
pub const SYSTEM_EVENT: &str = "jarvis://system-event";

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
        commands::start_chat,
        commands::cancel_chat,
        commands::report_ui_error,
    ])
}

/// Application entry point.
pub fn run() {
    let result = with_commands(tauri::Builder::default())
        .setup(setup)
        .build(tauri::generate_context!());

    match result {
        Ok(app) => app.run(|handle, event| {
            if let RunEvent::Exit = event
                && let Some(state) = handle.try_state::<AppState>()
            {
                state.shutdown();
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
