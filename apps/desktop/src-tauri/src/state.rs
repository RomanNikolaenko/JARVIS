use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jarvis_core::JarvisCore;
use jarvis_core::config::LogLevel;

use crate::logging::LoggingHandle;

/// State shared with IPC commands through `tauri::State`.
#[derive(Debug)]
pub struct AppState {
    core: Arc<JarvisCore>,
    logging: Option<LoggingHandle>,
    shut_down: AtomicBool,
}

impl AppState {
    pub fn new(core: Arc<JarvisCore>, logging: Option<LoggingHandle>) -> Self {
        Self {
            core,
            logging,
            shut_down: AtomicBool::new(false),
        }
    }

    pub fn core(&self) -> &JarvisCore {
        &self.core
    }

    pub fn apply_log_level(&self, level: LogLevel) {
        if let Some(logging) = &self.logging {
            logging.set_level(level);
        }
    }

    /// Stops background work (including the local model server) and
    /// flushes logs. Called on exit and before installing an update; only
    /// the first call does anything.
    pub async fn shutdown(&self) {
        if self.shut_down.swap(true, Ordering::SeqCst) {
            return;
        }
        self.core.shutdown().await;
        tracing::info!("JARVIS desktop shut down");
        if let Some(logging) = &self.logging {
            logging.flush();
        }
    }
}
