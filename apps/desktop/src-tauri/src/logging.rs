//! Diagnostic logging: `tracing` to stderr and to daily rotated files in the
//! platform log directory. The level comes from settings and can be changed
//! at runtime; `RUST_LOG` overrides it for debugging sessions.
//!
//! Never log secrets (API keys, tokens) or full conversation contents.

use std::error::Error;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use jarvis_core::config::LogLevel;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, fmt, reload};

const MAX_LOG_FILES: usize = 14;
const OWN_TARGETS: &[&str] = &[
    "jarvis_core",
    "jarvis_ai",
    "jarvis_events",
    "jarvis_platform",
    "jarvis_desktop",
    "jarvis_desktop_lib",
    "jarvis_ui",
];

pub struct LoggingHandle {
    filter: reload::Handle<EnvFilter, Registry>,
    /// Keeps the background log writer alive; taken on shutdown to flush.
    guard: Mutex<Option<WorkerGuard>>,
    /// `RUST_LOG` was set explicitly and takes precedence over settings.
    env_override: bool,
}

impl std::fmt::Debug for LoggingHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingHandle")
            .field("env_override", &self.env_override)
            .finish_non_exhaustive()
    }
}

impl LoggingHandle {
    /// Applies the log level from settings unless `RUST_LOG` overrides it.
    pub fn set_level(&self, level: LogLevel) {
        if self.env_override {
            return;
        }
        if let Err(err) = self.filter.reload(filter_for(level)) {
            tracing::warn!("cannot change log level: {err}");
        } else {
            tracing::info!("log level set to {}", level.as_str());
        }
    }

    /// Flushes buffered log lines. Further log lines go to stderr only.
    pub fn flush(&self) {
        let guard = self
            .guard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(guard);
    }
}

/// Installs the global subscriber. Can only succeed once per process.
pub fn init(log_dir: &Path, level: LogLevel) -> Result<LoggingHandle, Box<dyn Error>> {
    let env_override = std::env::var_os("RUST_LOG").is_some();
    let filter = if env_override {
        EnvFilter::try_from_default_env()?
    } else {
        filter_for(level)
    };
    let (filter_layer, filter_handle) = reload::Layer::new(filter);

    std::fs::create_dir_all(log_dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("jarvis")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(log_dir)?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(cfg!(debug_assertions)),
        )
        .with(fmt::layer().with_writer(file_writer).with_ansi(false))
        .try_init()?;

    install_panic_hook();

    Ok(LoggingHandle {
        filter: filter_handle,
        guard: Mutex::new(Some(guard)),
        env_override,
    })
}

/// Our crates log at `level`; dependencies only log warnings and errors.
fn filter_for(level: LogLevel) -> EnvFilter {
    let level = level.as_str();
    let directives: Vec<String> = std::iter::once("warn".to_owned())
        .chain(OWN_TARGETS.iter().map(|target| format!("{target}={level}")))
        .collect();
    EnvFilter::new(directives.join(","))
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(target: "jarvis_desktop", "panic: {info}");
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_limits_dependencies_to_warnings() {
        let filter = filter_for(LogLevel::Debug).to_string();
        let directives: Vec<&str> = filter.split(',').collect();
        assert!(directives.contains(&"warn"), "{filter}");
        for target in OWN_TARGETS {
            assert!(
                directives.contains(&format!("{target}=debug").as_str()),
                "{filter}"
            );
        }
    }
}
