//! Signed application updates (ADR 0014): checks the release feed, and on
//! the user's confirmation downloads, verifies and installs a new version.
//!
//! The plugin verifies every downloaded file against the public key built
//! into the app; a file whose signature does not match is never installed.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jarvis_core::{AvailableUpdate, ErrorCode, ErrorPayload, UpdatePhase, UpdateStatus};
use jarvis_events::EventCategory;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};
use tokio::sync::watch;

use crate::MAIN_WINDOW;
use crate::state::AppState;

/// Replaces the update feed URL (tests). Release builds accept HTTPS only;
/// the signature check always applies.
pub const UPDATE_ENDPOINT_ENV: &str = "JARVIS_UPDATE_ENDPOINT";
/// Payload: `UpdateStatus` on every change.
pub const UPDATE_STATUS_EVENT: &str = "jarvis://update-status";

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Progress is reported at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

type CommandResult<T> = Result<T, ErrorPayload>;

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn unavailable(message: impl Into<String>) -> ErrorPayload {
    ErrorPayload::new(ErrorCode::Unavailable, message.into())
}

/// The update state shared by commands, the automatic check and the UI.
pub struct UpdateService {
    status: watch::Sender<UpdateStatus>,
    /// The update found by the last check, kept for installing it.
    pending: Mutex<Option<Update>>,
    /// One check or installation at a time.
    busy: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for UpdateService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateService")
            .field("status", &*self.status.borrow())
            .finish_non_exhaustive()
    }
}

impl UpdateService {
    pub fn new(current_version: &str) -> Arc<Self> {
        Arc::new(Self {
            status: watch::channel(UpdateStatus::new(current_version)).0,
            pending: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
        })
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<UpdateStatus> {
        self.status.subscribe()
    }

    fn update_status(&self, change: impl FnOnce(&mut UpdateStatus)) {
        self.status.send_modify(change);
    }

    fn fail(&self, message: String) -> ErrorPayload {
        self.update_status(|s| {
            s.phase = UpdatePhase::Failed;
            s.error = Some(message.clone());
            s.downloaded_bytes = None;
            s.total_bytes = None;
        });
        unavailable(message)
    }

    fn updater<R: Runtime>(app: &AppHandle<R>) -> Result<Updater, String> {
        let mut builder = app
            .updater_builder()
            .timeout(REQUEST_TIMEOUT)
            // Like every HTTP client in JARVIS: no implicit proxy.
            .no_proxy();
        if let Some(endpoint) = std::env::var(UPDATE_ENDPOINT_ENV)
            .ok()
            .filter(|e| !e.trim().is_empty())
        {
            let url = tauri::Url::parse(endpoint.trim())
                .map_err(|err| format!("{UPDATE_ENDPOINT_ENV} is not a URL: {err}"))?;
            builder = builder
                .endpoints(vec![url])
                .map_err(|err| err.to_string())?;
        }
        builder.build().map_err(|err| err.to_string())
    }

    /// Asks the feed for a newer version.
    pub async fn check<R: Runtime>(&self, app: &AppHandle<R>) -> CommandResult<UpdateStatus> {
        let _busy = self.busy.lock().await;
        self.update_status(|s| {
            s.phase = UpdatePhase::Checking;
            s.error = None;
        });
        let updater = Self::updater(app).map_err(|err| self.fail(err))?;
        let found = updater
            .check()
            .await
            .map_err(|err| self.fail(format!("The update check failed: {err}")))?;
        let available = found.as_ref().map(|update| AvailableUpdate {
            version: update.version.clone(),
            notes: update
                .body
                .as_ref()
                .map(|notes| notes.chars().take(4000).collect()),
            date: update
                .raw_json
                .get("pub_date")
                .and_then(|d| d.as_str())
                .map(str::to_owned),
        });
        let announce = available.as_ref().map(|a| a.version.clone());
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = found;
        self.update_status(|s| {
            s.phase = if available.is_some() {
                UpdatePhase::Available
            } else {
                UpdatePhase::UpToDate
            };
            s.available = available;
            s.last_checked_ms = Some(unix_millis());
        });
        if let Some(version) = announce
            && let Some(state) = app.try_state::<AppState>()
        {
            state.core().events().info(
                EventCategory::Lifecycle,
                format!("JARVIS {version} is available (the update button in the top bar)"),
            );
        }
        Ok(self.status())
    }

    /// Downloads, verifies and installs the version the user confirmed, then
    /// restarts JARVIS. Running work is stopped first.
    pub async fn install<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        version: &str,
    ) -> CommandResult<()> {
        let _busy = self.busy.lock().await;
        let update = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .filter(|update| update.version == version)
            .ok_or_else(|| {
                ErrorPayload::new(
                    ErrorCode::Validation,
                    format!("Version {version} is not the update found last; check again"),
                )
            })?;
        self.update_status(|s| {
            s.phase = UpdatePhase::Downloading;
            s.error = None;
            s.downloaded_bytes = Some(0);
            s.total_bytes = None;
        });

        let mut downloaded: u64 = 0;
        let mut last_report = Instant::now();
        let bytes = update
            .download(
                |chunk, total| {
                    downloaded += chunk as u64;
                    if last_report.elapsed() >= PROGRESS_INTERVAL {
                        last_report = Instant::now();
                        self.update_status(|s| {
                            s.downloaded_bytes = Some(downloaded);
                            s.total_bytes = total;
                        });
                    }
                },
                || {},
            )
            .await
            .map_err(|err| self.fail(format!("The download failed: {err}")))?;
        // `download` verified the signature already; checked again before
        // anything is stopped, so a bad file never interrupts the user.
        update
            .verify(&bytes)
            .map_err(|err| self.fail(format!("The update's signature is not valid: {err}")))?;
        self.update_status(|s| {
            s.phase = UpdatePhase::Installing;
            s.downloaded_bytes = Some(bytes.len() as u64);
        });
        if let Some(state) = app.try_state::<AppState>() {
            state.core().events().info(
                EventCategory::Lifecycle,
                format!("Installing JARVIS {version}; JARVIS restarts"),
            );
        }

        if cfg!(windows) {
            // The installer replaces the running program: everything stops
            // first, then the plugin starts the installer and exits.
            shut_down(app).await;
            if let Err(err) = update.install(&bytes) {
                tracing::error!("installing the update failed: {err}");
                app.restart();
            }
            Ok(())
        } else {
            // Linux: the package or AppImage is replaced while JARVIS runs;
            // only a successful installation stops and restarts it.
            update
                .install(&bytes)
                .map_err(|err| self.fail(format!("Installing the update failed: {err}")))?;
            shut_down(app).await;
            app.restart();
        }
    }
}

async fn shut_down<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AppState>() {
        state.shutdown().await;
    }
}

/// Checks shortly after start and then daily, while the setting allows it.
pub fn spawn_automatic_checks<R: Runtime>(app: AppHandle<R>, service: Arc<UpdateService>) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let enabled = app
                .try_state::<AppState>()
                .is_some_and(|state| state.core().settings().updates.check_automatically);
            let idle = matches!(
                service.status().phase,
                UpdatePhase::Idle | UpdatePhase::UpToDate | UpdatePhase::Failed
            );
            if enabled
                && idle
                && let Err(err) = service.check(&app).await
            {
                tracing::info!("automatic update check failed: {}", err.message);
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// Sends the update status to the main window on every change.
pub fn spawn_status_forwarder<R: Runtime>(app: AppHandle<R>, service: &UpdateService) {
    let mut status = service.subscribe();
    tauri::async_runtime::spawn(async move {
        while status.changed().await.is_ok() {
            let current = status.borrow_and_update().clone();
            if let Err(err) = app.emit_to(MAIN_WINDOW, UPDATE_STATUS_EVENT, &current) {
                tracing::debug!("cannot forward the update status: {err}");
            }
        }
    });
}

#[tauri::command]
pub async fn get_update_status(
    service: State<'_, Arc<UpdateService>>,
) -> CommandResult<UpdateStatus> {
    Ok(service.status())
}

#[tauri::command]
pub async fn check_for_update<R: Runtime>(
    app: AppHandle<R>,
    service: State<'_, Arc<UpdateService>>,
) -> CommandResult<UpdateStatus> {
    service.check(&app).await
}

/// Installs the version the user confirmed (it must be the one found last).
#[tauri::command]
pub async fn install_update<R: Runtime>(
    app: AppHandle<R>,
    service: State<'_, Arc<UpdateService>>,
    version: String,
) -> CommandResult<()> {
    service.install(&app, &version).await
}
