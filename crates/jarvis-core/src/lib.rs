//! JARVIS core: the composition root of the domain layer.
//!
//! [`JarvisCore`] wires the subsystems (configuration, event bus, platform
//! layer, AI providers, chat orchestration) and exposes a small, UI-agnostic
//! API. It has no dependency on Tauri or any UI technology: the desktop shell
//! is just one adapter on top of it.

pub mod chat;
pub mod config;
pub mod error;
pub mod health;
mod ui_errors;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jarvis_ai::{AiProvider, EchoProvider, ModelInfo, ProviderDescriptor, ProviderRegistry};
use jarvis_events::{EventBus, EventCategory};
use jarvis_platform::{OsFamily, PlatformInfo, SystemMetrics, SystemMonitor, current_adapter};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use chat::{ChatEventSink, ChatService, ChatStartRequest, ChatStreamEvent, SinkClosed};
pub use config::{ConfigSource, ConfigStore, Settings};
pub use error::{CoreError, ErrorCode, ErrorPayload};
pub use health::{ComponentHealth, HealthReport, HealthStatus};
pub use ui_errors::UiErrorReport;

/// Timeout for listing the models of one provider.
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(5);

/// Filesystem locations used by JARVIS. Resolved by the host application
/// (e.g. via Tauri's path resolver) and passed in, so the core never guesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl AppPaths {
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum BuildProfile {
    Debug,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub build_profile: BuildProfile,
    pub os_family: OsFamily,
    pub arch: String,
    pub config_file: String,
    pub data_dir: String,
    pub log_dir: String,
    pub started_at_ms: u64,
}

/// Models offered by one provider, or the reason they could not be listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ProviderModels {
    pub provider: ProviderDescriptor,
    pub models: Vec<ModelInfo>,
    pub error: Option<String>,
}

/// Options for [`JarvisCore::initialize`].
#[derive(Debug)]
pub struct CoreOptions {
    pub paths: AppPaths,
    pub app_version: String,
    pub providers: Vec<Arc<dyn AiProvider>>,
}

impl CoreOptions {
    /// Default options: only the diagnostic echo provider is registered.
    pub fn new(paths: AppPaths, app_version: impl Into<String>) -> Self {
        Self {
            paths,
            app_version: app_version.into(),
            providers: vec![Arc::new(EchoProvider::new())],
        }
    }

    /// Replaces the registered AI providers.
    pub fn with_providers(mut self, providers: Vec<Arc<dyn AiProvider>>) -> Self {
        self.providers = providers;
        self
    }
}

#[derive(Debug)]
pub struct JarvisCore {
    paths: AppPaths,
    version: String,
    started: Instant,
    started_at_ms: u64,
    events: EventBus,
    config: Arc<ConfigStore>,
    platform: PlatformInfo,
    /// Created lazily: the first CPU measurement setup takes ~1.5 s on
    /// Windows, which must not delay startup.
    monitor: Arc<MonitorCell>,
    providers: Arc<ProviderRegistry>,
    chat: ChatService,
    ui_error_limiter: ui_errors::RateLimiter,
}

impl JarvisCore {
    /// Initializes all core subsystems. Must be called inside a Tokio runtime.
    pub async fn initialize(options: CoreOptions) -> Result<Arc<Self>, CoreError> {
        let started = Instant::now();
        let CoreOptions {
            paths,
            app_version,
            providers,
        } = options;

        for dir in [&paths.config_dir, &paths.data_dir, &paths.log_dir] {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|err| CoreError::io(format!("creating {}", dir.display()), err))?;
        }

        let events = EventBus::default();
        events.info(
            EventCategory::Lifecycle,
            format!("JARVIS core {app_version} starting"),
        );

        let config = Arc::new(ConfigStore::load(paths.config_file()).await);
        match config.source() {
            ConfigSource::Fallback { error } => events.warning(
                EventCategory::Config,
                format!("Settings file rejected, using defaults: {error}"),
            ),
            ConfigSource::CreatedDefaults => {
                events.info(EventCategory::Config, "Default settings created")
            }
            ConfigSource::File => events.info(EventCategory::Config, "Settings loaded"),
        };

        let platform =
            tokio::task::spawn_blocking(|| PlatformInfo::collect(current_adapter().as_ref()))
                .await
                .map_err(|err| CoreError::Internal(format!("platform probe failed: {err}")))?;

        // Warm up the resource monitor in the background.
        let monitor = Arc::new(MonitorCell::new());
        let warm_up = Arc::clone(&monitor);
        tokio::spawn(async move {
            if let Err(err) = monitor_handle(&warm_up).await {
                tracing::warn!("resource monitor unavailable: {err}");
            }
        });
        events.info(
            EventCategory::Platform,
            format!(
                "Platform: {} ({:?}, {})",
                platform.os_name.as_deref().unwrap_or("unknown OS"),
                platform.display_server,
                platform.arch
            ),
        );

        let registry = Arc::new(ProviderRegistry::new());
        for provider in providers {
            let name = provider.descriptor().display_name.clone();
            registry.register(provider)?;
            events.info(EventCategory::Ai, format!("AI provider registered: {name}"));
        }

        let chat = ChatService::new(
            Arc::clone(&registry),
            Arc::clone(&config),
            events.clone(),
            tokio::runtime::Handle::current(),
        );

        events.info(
            EventCategory::Lifecycle,
            format!("JARVIS core ready in {} ms", started.elapsed().as_millis()),
        );

        Ok(Arc::new(Self {
            paths,
            version: app_version,
            started,
            started_at_ms: unix_millis(),
            events,
            config,
            platform,
            monitor,
            providers: registry,
            chat,
            ui_error_limiter: ui_errors::RateLimiter::new(
                ui_errors::UI_ERRORS_PER_WINDOW,
                ui_errors::UI_ERROR_WINDOW,
            ),
        }))
    }

    pub fn info(&self) -> AppInfo {
        AppInfo {
            name: "JARVIS".to_owned(),
            version: self.version.clone(),
            build_profile: if cfg!(debug_assertions) {
                BuildProfile::Debug
            } else {
                BuildProfile::Release
            },
            os_family: self.platform.os_family,
            arch: self.platform.arch.clone(),
            config_file: self.paths.config_file().display().to_string(),
            data_dir: self.paths.data_dir.display().to_string(),
            log_dir: self.paths.log_dir.display().to_string(),
            started_at_ms: self.started_at_ms,
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn events(&self) -> &EventBus {
        &self.events
    }

    pub fn chat(&self) -> &ChatService {
        &self.chat
    }

    pub fn platform_info(&self) -> &PlatformInfo {
        &self.platform
    }

    pub fn settings(&self) -> Settings {
        self.config.current()
    }

    /// Validates, persists and activates new settings.
    pub async fn update_settings(&self, settings: Settings) -> Result<Settings, CoreError> {
        let saved = self.config.update(settings).await?;
        self.events.info(EventCategory::Config, "Settings saved");
        Ok(saved)
    }

    /// Samples CPU and memory usage on a blocking thread.
    pub async fn system_metrics(&self) -> Result<SystemMetrics, CoreError> {
        let monitor = monitor_handle(&self.monitor).await?;
        tokio::task::spawn_blocking(move || monitor.sample())
            .await
            .map_err(|err| CoreError::Internal(format!("metrics sampling failed: {err}")))
    }

    /// Lists models of every registered provider. Provider failures are
    /// reported per provider instead of failing the whole call.
    pub async fn list_models(&self) -> Vec<ProviderModels> {
        let lookups = self
            .providers
            .providers()
            .into_iter()
            .map(|provider| async move {
                let descriptor = provider.descriptor().clone();
                match tokio::time::timeout(LIST_MODELS_TIMEOUT, provider.list_models()).await {
                    Ok(Ok(models)) => ProviderModels {
                        provider: descriptor,
                        models,
                        error: None,
                    },
                    Ok(Err(err)) => ProviderModels {
                        provider: descriptor,
                        models: Vec::new(),
                        error: Some(err.to_string()),
                    },
                    Err(_) => ProviderModels {
                        provider: descriptor,
                        models: Vec::new(),
                        error: Some("listing models timed out".to_owned()),
                    },
                }
            });
        futures::future::join_all(lookups).await
    }

    /// Runs all component health probes concurrently.
    pub async fn health(&self) -> HealthReport {
        let config_source = self.config.source();
        let subscribers = self.events.subscriber_count();
        let display_server = self.platform.display_server;

        let (config, storage, events, platform, ai) = tokio::join!(
            health::run_probe("config", "Configuration", async {
                health::config_health(&config_source)
            }),
            health::run_probe(
                "storage",
                "Local storage",
                health::storage_health(&self.paths.data_dir)
            ),
            health::run_probe("events", "Event bus", async {
                (
                    HealthStatus::Healthy,
                    format!("{subscribers} live subscriber(s)"),
                )
            }),
            health::run_probe("platform", "Platform layer", async {
                (
                    HealthStatus::Healthy,
                    format!("Display server: {display_server:?}"),
                )
            }),
            health::run_probe(
                "ai",
                "AI providers",
                health::ai_health(self.providers.providers())
            ),
        );

        let components = vec![config, storage, events, platform, ai];
        HealthReport {
            status: HealthReport::aggregate(&components),
            version: self.version.clone(),
            uptime_secs: self.started.elapsed().as_secs(),
            checked_at_ms: unix_millis(),
            components,
        }
    }

    /// Records an error reported by the UI. Reports beyond the rate limit are
    /// dropped so a misbehaving UI cannot flood the logs.
    pub fn record_ui_error(&self, report: UiErrorReport) {
        if !self.ui_error_limiter.try_acquire() {
            tracing::debug!(target: "jarvis_ui", "UI error report dropped by rate limit");
            return;
        }
        let report = report.sanitized();
        let context = report.context.as_deref().unwrap_or("ui");
        tracing::error!(target: "jarvis_ui", context, "{}", report.message);
        self.events.error(
            EventCategory::Ui,
            format!("UI error ({context}): {}", report.message),
        );
    }

    /// Cancels running work. Safe to call more than once.
    pub fn shutdown(&self) {
        self.chat.cancel_all();
        self.events
            .info(EventCategory::Lifecycle, "JARVIS core shutting down");
    }
}

type MonitorCell = tokio::sync::OnceCell<Arc<SystemMonitor>>;

/// Returns the resource monitor, creating it on a blocking thread on first use.
/// Concurrent callers wait for the same initialization.
async fn monitor_handle(cell: &MonitorCell) -> Result<Arc<SystemMonitor>, CoreError> {
    cell.get_or_try_init(|| async {
        tokio::task::spawn_blocking(|| Arc::new(SystemMonitor::new()))
            .await
            .map_err(|err| CoreError::Internal(format!("resource monitor failed to start: {err}")))
    })
    .await
    .cloned()
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
