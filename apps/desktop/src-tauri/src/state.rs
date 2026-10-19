use std::sync::Arc;

use jarvis_core::JarvisCore;
use jarvis_core::config::LogLevel;

use crate::logging::LoggingHandle;

/// State shared with IPC commands through `tauri::State`.
#[derive(Debug)]
pub struct AppState {
    core: Arc<JarvisCore>,
    logging: Option<LoggingHandle>,
}

impl AppState {
    pub fn new(core: Arc<JarvisCore>, logging: Option<LoggingHandle>) -> Self {
        Self { core, logging }
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
    /// flushes logs. Called once on exit.
    pub async fn shutdown(&self) {
        self.core.shutdown().await;
        tracing::info!("JARVIS desktop shut down");
        if let Some(logging) = &self.logging {
            logging.flush();
        }
    }
}
