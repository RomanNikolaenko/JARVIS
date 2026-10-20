//! JARVIS core: the composition root of the domain layer.
//!
//! [`JarvisCore`] wires the subsystems (configuration, event bus, platform
//! layer, AI providers, chat orchestration, memory, agents) and exposes a small, UI-agnostic
//! API. It has no dependency on Tauri or any UI technology: the desktop shell
//! is just one adapter on top of it.

mod agents;
pub mod catalog;
pub mod chat;
pub mod config;
pub mod error;
pub mod health;
mod memory;
pub mod routing;
mod ui_errors;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use jarvis_ai::{
    AiError, AiProvider, DataLocality, EchoProvider, HttpClient, HttpProviderFactory, ModelInfo,
    ProviderConfig, ProviderFactory, ProviderRegistry,
};
use jarvis_events::{EventBus, EventCategory};
use jarvis_memory::MemoryEngine;
use jarvis_platform::{OsFamily, PlatformInfo, SystemMetrics, SystemMonitor, current_adapter};
use jarvis_runtime::process::CommandFactory;
use jarvis_runtime::{RuntimeManager, RuntimeOptions, ServerRole};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::catalog::ModelCatalog;

pub use agents::{AgentStartRequest, AgentStatus, LaunchedProgram, ToolInfo, WorkspaceFolder};
pub use catalog::ProviderModels;
pub use chat::{ChatEventSink, ChatService, ChatStartRequest, ChatStreamEvent, SinkClosed};
pub use config::{ConfigSource, ConfigStore, Settings};
pub use error::{CoreError, ErrorCode, ErrorPayload};
pub use health::{ComponentHealth, HealthReport, HealthStatus};
pub use jarvis_memory;
pub use memory::{ChatTurnRequest, MAX_IMPORT_BYTES, MAX_TURN_CHARS};
pub use routing::{ModelRef, ModelRouting, RouteResolution, TaskCategory};
pub use ui_errors::UiErrorReport;
pub use {jarvis_agent, jarvis_security, jarvis_tools};

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

/// Result of probing a provider configuration without saving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ProviderTestResult {
    pub reachable: bool,
    /// Privacy class derived from the URL.
    pub locality: DataLocality,
    pub latency_ms: u32,
    pub models: Vec<ModelInfo>,
    pub error: Option<String>,
}

/// Options for [`JarvisCore::initialize`].
pub struct CoreOptions {
    pub paths: AppPaths,
    pub app_version: String,
    /// Providers that exist regardless of settings (the diagnostic provider).
    pub static_providers: Vec<Arc<dyn AiProvider>>,
    /// Builds providers from `ai.providers`; `None` disables configured
    /// providers (used by tests that must not touch the network).
    pub provider_factory: Option<Arc<dyn ProviderFactory>>,
    /// Replaces the `llama-server` command of the local runtime (tests).
    pub runtime_command_factory: Option<CommandFactory>,
}

impl std::fmt::Debug for CoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreOptions")
            .field("paths", &self.paths)
            .field("app_version", &self.app_version)
            .finish_non_exhaustive()
    }
}

impl CoreOptions {
    /// Production defaults: the diagnostic echo provider plus HTTP providers
    /// from settings.
    pub fn new(paths: AppPaths, app_version: impl Into<String>) -> Self {
        Self {
            paths,
            app_version: app_version.into(),
            static_providers: vec![Arc::new(EchoProvider::new())],
            provider_factory: Some(Arc::new(HttpProviderFactory::new())),
            runtime_command_factory: None,
        }
    }

    pub fn with_runtime_command_factory(mut self, factory: CommandFactory) -> Self {
        self.runtime_command_factory = Some(factory);
        self
    }

    /// Replaces the providers that exist regardless of settings.
    pub fn with_static_providers(mut self, providers: Vec<Arc<dyn AiProvider>>) -> Self {
        self.static_providers = providers;
        self
    }

    pub fn with_provider_factory(mut self, factory: Option<Arc<dyn ProviderFactory>>) -> Self {
        self.provider_factory = factory;
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
    static_providers: Vec<Arc<dyn AiProvider>>,
    provider_factory: Option<Arc<dyn ProviderFactory>>,
    providers: Arc<ProviderRegistry>,
    /// Serialises registry rebuilds (settings changes, runtime start/stop).
    provider_reload: tokio::sync::Mutex<()>,
    catalog: Arc<ModelCatalog>,
    runtime: Arc<RuntimeManager>,
    chat: ChatService,
    memory: Arc<MemoryEngine>,
    memory_state: Arc<memory::MemoryState>,
    agents: agents::AgentSystem,
    ui_error_limiter: ui_errors::RateLimiter,
}

impl JarvisCore {
    /// Initializes all core subsystems. Must be called inside a Tokio runtime.
    /// Does not contact AI servers; they are probed on demand.
    pub async fn initialize(options: CoreOptions) -> Result<Arc<Self>, CoreError> {
        let started = Instant::now();
        let CoreOptions {
            paths,
            app_version,
            static_providers,
            provider_factory,
            runtime_command_factory,
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

        let runtime = RuntimeManager::new(RuntimeOptions {
            data_dir: paths.data_dir.clone(),
            events: events.clone(),
            ai_http: Arc::new(HttpClient::new()),
            command_factory: runtime_command_factory,
        })
        .await?;

        let registry = Arc::new(ProviderRegistry::new());
        registry.replace_all(build_providers(
            &static_providers,
            runtime.providers(),
            provider_factory.as_deref(),
            &config.current().ai.providers,
            &events,
        ))?;
        events.info(
            EventCategory::Ai,
            format!("AI providers: {}", provider_names(&registry)),
        );

        let memory =
            Arc::new(MemoryEngine::open(paths.data_dir.join("memory").join("jarvis.db")).await);
        match memory.unavailable_reason() {
            Some(reason) => events.error(
                EventCategory::Memory,
                format!("Memory database unavailable, only temporary chats work: {reason}"),
            ),
            None => {
                let counts = memory.counts().await.unwrap_or_default();
                events.info(
                    EventCategory::Memory,
                    format!(
                        "Memory: {} conversations, {} facts, {} projects",
                        counts.conversations, counts.facts, counts.projects
                    ),
                )
            }
        };

        let agents =
            agents::AgentSystem::open(&memory, Arc::clone(&config), &paths, &events).await?;

        let catalog = Arc::new(ModelCatalog::default());
        let chat = ChatService::new(
            Arc::clone(&registry),
            Arc::clone(&catalog),
            Arc::clone(&config),
            events.clone(),
            tokio::runtime::Handle::current(),
        );

        events.info(
            EventCategory::Lifecycle,
            format!("JARVIS core ready in {} ms", started.elapsed().as_millis()),
        );

        let core = Arc::new(Self {
            paths,
            version: app_version,
            started,
            started_at_ms: unix_millis(),
            events,
            config,
            platform,
            monitor,
            static_providers,
            provider_factory,
            providers: registry,
            provider_reload: tokio::sync::Mutex::new(()),
            catalog,
            runtime,
            chat,
            memory,
            memory_state: Arc::default(),
            agents,
            ui_error_limiter: ui_errors::RateLimiter::new(
                ui_errors::UI_ERRORS_PER_WINDOW,
                ui_errors::UI_ERROR_WINDOW,
            ),
        });
        Self::watch_runtime_provider(&core);
        Self::autostart_runtime(&core);
        Self::spawn_memory_tasks(&core);
        Self::spawn_agent_events(&core);
        Ok(core)
    }

    /// Re-registers providers whenever a server of the embedded runtime
    /// starts or stops serving a model.
    fn watch_runtime_provider(core: &Arc<Self>) {
        for role in ServerRole::ALL {
            let mut receiver = core.runtime.subscribe_provider(role);
            let weak = Arc::downgrade(core);
            tokio::spawn(async move {
                while receiver.changed().await.is_ok() {
                    let Some(core) = weak.upgrade() else { break };
                    if let Err(err) = core.reload_providers().await {
                        tracing::warn!("cannot reload AI providers: {err}");
                    }
                }
            });
        }
    }

    /// Starts the configured models, if the user enabled autostart. The
    /// embedding model goes first: it is small and loads in seconds, and the
    /// memory check of the chat model then sees what it uses.
    fn autostart_runtime(core: &Arc<Self>) {
        let settings = core.settings().runtime;
        let models: Vec<(ServerRole, String)> = [
            (ServerRole::Embeddings, settings.autostart_embedding_model),
            (ServerRole::Chat, settings.autostart_model),
        ]
        .into_iter()
        .filter_map(|(role, model)| Some((role, model?)))
        .collect();
        if models.is_empty() {
            return;
        }
        if core.runtime.binary().is_none() {
            for (_, model) in &models {
                core.events.warning(
                    EventCategory::Runtime,
                    format!("Autostart of {model} skipped: the llama.cpp runtime is not installed"),
                );
            }
            return;
        }
        let core = Arc::clone(core);
        tokio::spawn(async move {
            let timeout = std::time::Duration::from_secs(u64::from(settings.startup_timeout_secs));
            for (role, model) in models {
                match core.start_local_model(role, &model).await {
                    Ok(()) => {
                        core.runtime.wait_settled(role, timeout).await;
                    }
                    Err(err) => {
                        core.events.error(
                            EventCategory::Runtime,
                            format!("Autostart of {model} failed: {err}"),
                        );
                    }
                }
            }
        });
    }

    /// Rebuilds the registry from settings and the runtime state.
    async fn reload_providers(&self) -> Result<(), CoreError> {
        let _guard = self.provider_reload.lock().await;
        let current = self.config.current();
        self.providers.replace_all(build_providers(
            &self.static_providers,
            self.runtime.providers(),
            self.provider_factory.as_deref(),
            &current.ai.providers,
            &self.events,
        ))?;
        self.catalog.invalidate().await;
        self.events.info(
            EventCategory::Ai,
            format!("AI providers: {}", provider_names(&self.providers)),
        );
        // A new or vanished embedding model changes what can be indexed.
        self.notify_indexer();
        Ok(())
    }

    /// The embedded llama.cpp runtime.
    pub fn runtime(&self) -> &Arc<RuntimeManager> {
        &self.runtime
    }

    /// Starts serving a library model in the role's server with the runtime
    /// settings.
    pub async fn start_local_model(
        &self,
        role: ServerRole,
        model_id: &str,
    ) -> Result<(), CoreError> {
        let settings = self.config.current().runtime;
        Ok(self.runtime.start(role, model_id, &settings).await?)
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

    /// Validates, persists and activates new settings. Provider changes take
    /// effect immediately; running chats keep their provider.
    pub async fn update_settings(&self, settings: Settings) -> Result<Settings, CoreError> {
        let previous = self.config.current();
        let saved = self.config.update(settings).await?;
        self.events.info(EventCategory::Config, "Settings saved");

        if previous.ai.providers != saved.ai.providers {
            // Rebuilds from the latest saved settings, so concurrent updates
            // cannot leave an older provider set active.
            self.reload_providers().await?;
        }
        Ok(saved)
    }

    /// Samples CPU and memory usage on a blocking thread.
    pub async fn system_metrics(&self) -> Result<SystemMetrics, CoreError> {
        let monitor = monitor_handle(&self.monitor).await?;
        tokio::task::spawn_blocking(move || monitor.sample())
            .await
            .map_err(|err| CoreError::Internal(format!("metrics sampling failed: {err}")))
    }

    /// Models of every registered provider (cached briefly; `refresh` forces
    /// a new query). Provider failures are reported per provider.
    pub async fn list_models(&self, refresh: bool) -> Vec<ProviderModels> {
        self.catalog.get(&self.providers, refresh).await.to_vec()
    }

    /// How each task category is currently routed.
    pub async fn resolve_routes(&self) -> Vec<RouteResolution> {
        let settings = self.config.current();
        let catalog = self.catalog.get(&self.providers, false).await;
        routing::resolve_all(
            &settings.ai.routing,
            &catalog,
            settings.privacy.allow_cloud_providers,
        )
    }

    /// Connects to a provider configuration without saving it and lists its
    /// models. No conversation data is sent.
    pub async fn test_provider(
        &self,
        config: ProviderConfig,
    ) -> Result<ProviderTestResult, CoreError> {
        config.validate().map_err(CoreError::Validation)?;
        let factory = self
            .provider_factory
            .as_ref()
            .ok_or_else(|| AiError::Unsupported {
                provider: config.id.clone(),
                operation: "connection test",
            })?;
        let provider = factory.create(&config)?;
        let locality = provider.descriptor().locality;

        let started = Instant::now();
        let result =
            tokio::time::timeout(catalog::LIST_MODELS_TIMEOUT, provider.list_models()).await;
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let (models, error) = match result {
            Ok(Ok(models)) => (models, None),
            Ok(Err(err)) => (Vec::new(), Some(err.to_string())),
            Err(_) => (Vec::new(), Some("connection test timed out".to_owned())),
        };
        self.events.info(
            EventCategory::Ai,
            match &error {
                None => format!(
                    "Provider test \"{}\": {} model(s) in {latency_ms} ms",
                    config.display_name,
                    models.len()
                ),
                Some(err) => format!("Provider test \"{}\" failed: {err}", config.display_name),
            },
        );
        Ok(ProviderTestResult {
            reachable: error.is_none(),
            locality,
            latency_ms,
            models,
            error,
        })
    }

    /// Runs all component health probes concurrently.
    pub async fn health(&self) -> HealthReport {
        let config_source = self.config.source();
        let subscribers = self.events.subscriber_count();
        let display_server = self.platform.display_server;

        let runtime_installed = self.runtime.binary().is_some();
        let runtime_state = self.runtime.server_state(ServerRole::Chat);
        let embedding_state = self.runtime.server_state(ServerRole::Embeddings);
        let saving = self.config.current().memory.enabled;
        let (config, storage, events, platform, ai, runtime, memory) = tokio::join!(
            health::run_probe("config", "Configuration", health::PROBE_TIMEOUT, async {
                health::config_health(&config_source)
            }),
            health::run_probe(
                "storage",
                "Local storage",
                health::PROBE_TIMEOUT,
                health::storage_health(&self.paths.data_dir)
            ),
            health::run_probe("events", "Event bus", health::PROBE_TIMEOUT, async {
                (
                    HealthStatus::Healthy,
                    format!("{subscribers} live subscriber(s)"),
                )
            }),
            health::run_probe("platform", "Platform layer", health::PROBE_TIMEOUT, async {
                (
                    HealthStatus::Healthy,
                    format!("Display server: {display_server:?}"),
                )
            }),
            health::run_probe(
                "ai",
                "AI providers",
                health::AI_PROBE_TIMEOUT,
                health::ai_health(self.providers.providers())
            ),
            health::run_probe("runtime", "Local runtime", health::PROBE_TIMEOUT, async {
                health::runtime_health(runtime_installed, &runtime_state, &embedding_state)
            }),
            health::run_probe(
                "memory",
                "Memory",
                health::PROBE_TIMEOUT,
                health::memory_health(&self.memory, saving)
            ),
        );

        let components = vec![config, storage, events, platform, ai, runtime, memory];
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

    /// Cancels running work and stops the local model server. Safe to call
    /// more than once.
    pub async fn shutdown(&self) {
        self.events
            .info(EventCategory::Lifecycle, "JARVIS core shutting down");
        // Running agent tasks pause and can be resumed after a restart.
        self.agents.manager.shutdown().await;
        self.chat.cancel_all();
        self.runtime.shutdown().await;
        // Last: the action history records the steps above.
        self.stop_memory_tasks().await;
    }
}

/// Static providers, the embedded runtime's servers (while serving) and the
/// enabled configured providers, in configuration order. Invalid
/// configurations are skipped with a warning.
fn build_providers(
    static_providers: &[Arc<dyn AiProvider>],
    runtime: Vec<Arc<dyn AiProvider>>,
    factory: Option<&dyn ProviderFactory>,
    configs: &[ProviderConfig],
    events: &EventBus,
) -> Vec<Arc<dyn AiProvider>> {
    let mut providers = static_providers.to_vec();
    // The embedded runtime comes before configured servers, so automatic
    // routing (and the choice of embedding model) prefers a model the user
    // started inside JARVIS.
    providers.extend(runtime);
    if let Some(factory) = factory {
        for config in configs.iter().filter(|c| c.enabled) {
            match factory.create(config) {
                Ok(provider) => providers.push(provider),
                Err(err) => {
                    events.warning(
                        EventCategory::Ai,
                        format!("AI provider \"{}\" skipped: {err}", config.display_name),
                    );
                }
            }
        }
    }
    providers
}

fn provider_names(registry: &ProviderRegistry) -> String {
    registry
        .descriptors()
        .into_iter()
        .map(|d| d.display_name)
        .collect::<Vec<_>>()
        .join(", ")
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

pub(crate) fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
