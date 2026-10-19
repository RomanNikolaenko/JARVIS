//! Orchestrates the local runtime: installation, model library, downloads
//! and the supervised `llama-server` process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use jarvis_ai::{
    AiProvider, DataLocality, HttpClient, OpenAiCompatibleProvider, ProviderDescriptor, ProviderId,
    ProviderKind,
};
use jarvis_events::{EventBus, EventCategory};
use jarvis_platform::{HardwareInfo, PlatformAdapter, current_adapter};
use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::catalog::{self, ResolvedFile};
use crate::download::{self, DownloadHandle, Downloads, Expected};
use crate::error::{Result, RuntimeError};
use crate::gguf;
use crate::installer;
use crate::library::{self, ModelLibrary, now_ms};
use crate::process::{self, CommandFactory, LaunchPlan, LogTail};
use crate::types::{
    BinarySource, CuratedModel, DownloadKind, DownloadPhase, FileDigest, GpuOffload, LocalModel,
    ModelDownloadRequest, ModelSource, RuntimeBackend, RuntimeBinary, RuntimeRelease,
    RuntimeSettings, RuntimeStatus, ServerPhase, ServerState,
};

/// Provider id of the embedded runtime in the AI registry.
pub const RUNTIME_PROVIDER_ID: &str = "jarvis-runtime";
const INSTALL_FILE: &str = "install.json";
const PID_FILE: &str = "server.pid";
const GRACEFUL_STOP: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const ALL_LAYERS: u32 = 999;
const LOG_TAIL_IN_STATUS: usize = 40;

#[derive(Debug, Serialize, Deserialize)]
struct PidRecord {
    pid: u32,
    exe: String,
    port: u16,
}

/// Options for [`RuntimeManager::new`].
pub struct RuntimeOptions {
    pub data_dir: PathBuf,
    pub events: EventBus,
    /// HTTP client shared with the other AI providers.
    pub ai_http: Arc<HttpClient>,
    /// Replaces the `llama-server` command (tests).
    pub command_factory: Option<CommandFactory>,
}

struct Supervisor {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

pub struct RuntimeManager {
    runtime_dir: PathBuf,
    library: ModelLibrary,
    downloads: Downloads,
    client: reqwest::Client,
    adapter: Box<dyn PlatformAdapter>,
    events: EventBus,
    ai_http: Arc<HttpClient>,
    command_factory: CommandFactory,
    binary: RwLock<Option<RuntimeBinary>>,
    state: watch::Sender<ServerState>,
    provider: watch::Sender<Option<Arc<dyn AiProvider>>>,
    logs: LogTail,
    supervisor: tokio::sync::Mutex<Option<Supervisor>>,
}

impl std::fmt::Debug for RuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeManager")
            .field("runtime_dir", &self.runtime_dir)
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl RuntimeManager {
    /// Loads the installation record and model library and removes a server
    /// left running by a previous session. Does not start anything.
    pub async fn new(options: RuntimeOptions) -> Result<Arc<Self>> {
        let runtime_dir = options.data_dir.join("runtime");
        tokio::fs::create_dir_all(&runtime_dir)
            .await
            .map_err(|err| RuntimeError::io("creating the runtime directory", err))?;
        let library = ModelLibrary::load(options.data_dir.join("models")).await?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("JARVIS/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| RuntimeError::Internal(format!("HTTP client: {err}")))?;

        let manager = Arc::new(Self {
            binary: RwLock::new(load_install_record(&runtime_dir).await),
            runtime_dir,
            library,
            downloads: Downloads::default(),
            client,
            adapter: current_adapter(),
            events: options.events,
            ai_http: options.ai_http,
            command_factory: options
                .command_factory
                .unwrap_or_else(process::default_command_factory),
            state: watch::Sender::new(ServerState::stopped()),
            provider: watch::Sender::new(None),
            logs: LogTail::default(),
            supervisor: tokio::sync::Mutex::new(None),
        });
        manager.cleanup_orphan().await;
        Ok(manager)
    }

    // ---- Observation --------------------------------------------------

    pub fn subscribe_provider(&self) -> watch::Receiver<Option<Arc<dyn AiProvider>>> {
        self.provider.subscribe()
    }

    /// The embedded provider while a model is being served.
    pub fn provider(&self) -> Option<Arc<dyn AiProvider>> {
        self.provider.borrow().clone()
    }

    pub fn subscribe_state(&self) -> watch::Receiver<ServerState> {
        self.state.subscribe()
    }

    pub fn server_state(&self) -> ServerState {
        self.state.borrow().clone()
    }

    pub fn binary(&self) -> Option<RuntimeBinary> {
        self.binary
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub async fn status(&self) -> RuntimeStatus {
        let mut server = self.server_state();
        if let Some(pid) = server.pid
            && matches!(server.phase, ServerPhase::Starting | ServerPhase::Running)
        {
            server.memory_bytes =
                tokio::task::spawn_blocking(move || jarvis_platform::process_memory(pid))
                    .await
                    .ok()
                    .flatten();
        }
        RuntimeStatus {
            binary: self.binary(),
            server,
            models: self.library.list(),
            downloads: self.downloads.snapshot(),
            models_dir: self.library.dir().display().to_string(),
            log_tail: self.logs.last(LOG_TAIL_IN_STATUS),
        }
    }

    pub async fn hardware() -> HardwareInfo {
        tokio::task::spawn_blocking(|| HardwareInfo::detect(current_adapter().as_ref()))
            .await
            .unwrap_or_else(|_| HardwareInfo::detect(current_adapter().as_ref()))
    }

    pub fn curated_models() -> Vec<CuratedModel> {
        catalog::curated_models()
    }

    /// Waits until the server is no longer starting or stopping.
    pub async fn wait_settled(&self, timeout: Duration) -> ServerState {
        let mut receiver = self.state.subscribe();
        let _ = tokio::time::timeout(
            timeout,
            receiver
                .wait_for(|s| !matches!(s.phase, ServerPhase::Starting | ServerPhase::Stopping)),
        )
        .await;
        self.server_state()
    }

    // ---- Installation --------------------------------------------------

    pub async fn check_release(&self, backend: RuntimeBackend) -> Result<RuntimeRelease> {
        installer::find_latest(&self.client, backend).await
    }

    /// Installs a release the user approved. Returns the download id; the
    /// result is reported through the download status and events.
    pub async fn install(self: &Arc<Self>, approved: RuntimeRelease) -> Result<String> {
        if self.downloads.has_active(DownloadKind::Runtime) {
            return Err(RuntimeError::InvalidInput(
                "an installation is already running".into(),
            ));
        }
        let release = installer::verify_approved(&self.client, &approved).await?;
        let handle = self.downloads.begin(
            DownloadKind::Runtime,
            format!("llama.cpp {} ({:?})", release.tag, release.backend),
        );
        let id = handle.id().to_owned();
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = this.install_release(&release, &handle).await;
            let message = match &result {
                Ok(binary) => format!("llama.cpp {} installed ({})", release.tag, binary.version),
                Err(RuntimeError::Cancelled) => "Runtime installation cancelled".to_owned(),
                Err(err) => format!("Runtime installation failed: {err}"),
            };
            if result
                .as_ref()
                .is_err_and(|e| !matches!(e, RuntimeError::Cancelled))
            {
                this.events.error(EventCategory::Runtime, message);
            } else {
                this.events.info(EventCategory::Runtime, message);
            }
            handle.finish(&result.map(|_| ()));
        });
        Ok(id)
    }

    async fn install_release(
        &self,
        release: &RuntimeRelease,
        handle: &DownloadHandle,
    ) -> Result<RuntimeBinary> {
        let url = installer::download_url(release)?;
        download::check_source(&url)?;
        let archive = self.runtime_dir.join("downloads").join(&release.asset_name);
        download::fetch_verified(
            &self.client,
            &url,
            &archive,
            &Expected {
                sha256: release.sha256.clone(),
                size: Some(release.size_bytes),
            },
            handle,
        )
        .await?;

        handle.phase(DownloadPhase::Installing);
        let backend_name = format!("{:?}", release.backend).to_ascii_lowercase();
        let install_root = self.runtime_dir.join("llama.cpp");
        let final_dir = install_root.join(format!("{}-{backend_name}", release.tag));
        let staging = install_root.join(format!("{}-{backend_name}.partial", release.tag));
        if let Some(current) = self.binary()
            && Path::new(&current.path).starts_with(&final_dir)
            && self.provider().is_some()
        {
            return Err(RuntimeError::InvalidInput(
                "this build is in use; stop the local model first".into(),
            ));
        }
        let _ = tokio::fs::remove_dir_all(&staging).await;
        {
            let (archive, staging) = (archive.clone(), staging.clone());
            tokio::task::spawn_blocking(move || installer::extract_archive(&archive, &staging))
                .await
                .map_err(|err| RuntimeError::Internal(err.to_string()))??;
        }
        let _ = tokio::fs::remove_dir_all(&final_dir).await;
        tokio::fs::rename(&staging, &final_dir)
            .await
            .map_err(|err| RuntimeError::io("activating the runtime", err))?;
        let _ = tokio::fs::remove_file(&archive).await;

        let server = installer::locate_server(&final_dir).ok_or_else(|| {
            RuntimeError::InvalidInput("the archive does not contain llama-server".into())
        })?;
        make_executable(&server)?;
        let version = installer::probe_version(&server, self.adapter.as_ref()).await?;
        let binary = RuntimeBinary {
            path: server.display().to_string(),
            version,
            backend: release.backend,
            source: BinarySource::Release {
                tag: release.tag.clone(),
                asset: release.asset_name.clone(),
                url: release.download_url.clone(),
            },
            sha256: download::sha256_file(&server).await?,
            libraries: installer::library_digests(&server).await?,
            installed_at_ms: now_ms(),
        };
        self.save_binary(binary.clone()).await?;
        self.remove_old_installs(&final_dir).await;
        Ok(binary)
    }

    /// Uses an existing `llama-server` chosen by the user.
    pub async fn select_binary(&self, path: &Path) -> Result<RuntimeBinary> {
        let path = library::canonical(path)
            .await
            .map_err(|err| RuntimeError::io(format!("opening {}", path.display()), err))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !name.starts_with("llama-server") {
            return Err(RuntimeError::InvalidInput(
                "select the llama-server executable of a llama.cpp build".into(),
            ));
        }
        let version = installer::probe_version(&path, self.adapter.as_ref()).await?;
        let binary = RuntimeBinary {
            backend: installer::detect_backend(&path),
            path: path.display().to_string(),
            version,
            source: BinarySource::UserSelected,
            sha256: download::sha256_file(&path).await?,
            libraries: installer::library_digests(&path).await?,
            installed_at_ms: now_ms(),
        };
        self.save_binary(binary.clone()).await?;
        self.events.info(
            EventCategory::Runtime,
            format!("Using llama-server at {} ({})", binary.path, binary.version),
        );
        Ok(binary)
    }

    async fn save_binary(&self, binary: RuntimeBinary) -> Result<()> {
        let json = serde_json::to_vec_pretty(&binary)
            .map_err(|err| RuntimeError::Internal(err.to_string()))?;
        library::write_atomic(&self.runtime_dir.join(INSTALL_FILE), &json).await?;
        *self.binary.write().unwrap_or_else(PoisonError::into_inner) = Some(binary);
        Ok(())
    }

    async fn remove_old_installs(&self, keep: &Path) {
        let Ok(mut entries) = tokio::fs::read_dir(self.runtime_dir.join("llama.cpp")).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path != keep && path.is_dir() {
                // Best effort: a running server keeps its files locked on Windows.
                let _ = tokio::fs::remove_dir_all(&path).await;
            }
        }
    }

    // ---- Models ----------------------------------------------------------

    pub async fn import_model(&self, path: &Path) -> Result<LocalModel> {
        let model = self.library.import(path).await?;
        self.events.info(
            EventCategory::Runtime,
            format!("Model imported: {}", model.display_name),
        );
        Ok(model)
    }

    pub async fn download_curated(self: &Arc<Self>, id: &str) -> Result<String> {
        let model = catalog::curated_models()
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| RuntimeError::NotFound(format!("unknown catalog model \"{id}\"")))?;
        let resolved = ResolvedFile::from_curated(&model)?;
        self.start_download(resolved, model.display_name)
    }

    pub async fn download_model(self: &Arc<Self>, request: ModelDownloadRequest) -> Result<String> {
        let resolved = catalog::resolve(&self.client, &request).await?;
        let label = format!("{} / {}", resolved.repo, resolved.file);
        self.start_download(resolved, label)
    }

    fn start_download(self: &Arc<Self>, resolved: ResolvedFile, label: String) -> Result<String> {
        download::check_source(&resolved.url)?;
        let dest = self.library.download_path(resolved.file_name());
        let already = self.library.list().into_iter().any(|m| {
            Path::new(&m.path) == dest.as_path()
                && m.available
                && m.sha256.as_deref() == Some(&resolved.expected.sha256)
        });
        if already {
            return Err(RuntimeError::InvalidInput(format!(
                "{label} is already downloaded"
            )));
        }
        if self
            .downloads
            .snapshot()
            .iter()
            .any(|d| d.label == label && !d.phase.is_finished())
        {
            return Err(RuntimeError::InvalidInput(format!(
                "{label} is already downloading"
            )));
        }
        let handle = self.downloads.begin(DownloadKind::Model, label.clone());
        let id = handle.id().to_owned();
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = async {
                download::fetch_verified(
                    &this.client,
                    &resolved.url,
                    &dest,
                    &resolved.expected,
                    &handle,
                )
                .await?;
                let source = ModelSource::Downloaded {
                    repo: resolved.repo.clone(),
                    file: resolved.file.clone(),
                    revision: resolved.revision.clone(),
                };
                this.library
                    .add_downloaded(dest.clone(), source, resolved.expected.sha256.clone())
                    .await
            }
            .await;
            match &result {
                Ok(model) => {
                    this.events.info(
                        EventCategory::Runtime,
                        format!("Model downloaded and verified: {}", model.display_name),
                    );
                }
                Err(RuntimeError::Cancelled) => {
                    this.events.info(
                        EventCategory::Runtime,
                        format!("Download cancelled: {label}"),
                    );
                }
                Err(err) => {
                    this.events.error(
                        EventCategory::Runtime,
                        format!("Download failed ({label}): {err}"),
                    );
                }
            }
            handle.finish(&result.map(|_| ()));
        });
        Ok(id)
    }

    pub fn cancel_download(&self, id: &str) -> bool {
        self.downloads.cancel(id)
    }

    pub async fn remove_model(&self, id: &str, delete_file: bool) -> Result<()> {
        let state = self.server_state();
        if state.model_id.as_deref() == Some(id)
            && matches!(state.phase, ServerPhase::Starting | ServerPhase::Running)
        {
            return Err(RuntimeError::InvalidInput(
                "stop the model before removing it".into(),
            ));
        }
        self.library.remove(id, delete_file).await?;
        self.events
            .info(EventCategory::Runtime, format!("Model removed: {id}"));
        Ok(())
    }

    // ---- Server lifecycle -------------------------------------------------

    /// Starts serving `model_id` (restarting if another model runs). Returns
    /// once the process is spawned; readiness is reported via the state.
    pub async fn start(self: &Arc<Self>, model_id: &str, settings: &RuntimeSettings) -> Result<()> {
        let mut supervisor = self.supervisor.lock().await;
        if let Some(previous) = supervisor.take() {
            self.stop_supervisor(previous).await;
        }

        let binary = self.binary().ok_or(RuntimeError::NotInstalled)?;
        let binary_path = PathBuf::from(&binary.path);
        let actual = download::sha256_file(&binary_path)
            .await
            .map_err(|_| RuntimeError::BinaryChanged(binary_path.clone()))?;
        if actual != binary.sha256 {
            return Err(RuntimeError::BinaryChanged(binary_path));
        }
        let libraries = installer::library_digests(&binary_path)
            .await
            .map_err(|_| RuntimeError::BinaryChanged(binary_path.clone()))?;
        if let Some(name) = changed_library(&binary.libraries, &libraries) {
            return Err(RuntimeError::BinaryChanged(
                binary_path.with_file_name(name),
            ));
        }
        let model = self.library.get(model_id).ok_or_else(|| {
            RuntimeError::NotFound(format!("model \"{model_id}\" is not in the library"))
        })?;
        if !model.available {
            return Err(RuntimeError::NotFound(format!(
                "the file of \"{}\" is missing or changed: {}",
                model.display_name, model.path
            )));
        }

        let hardware = Self::hardware().await;
        let gpu_layers = gpu_layers(settings, binary.backend, &hardware, &model);
        let estimate = gguf::estimate_memory(model.size_bytes, &model.gguf, settings.context_size);
        let gpu_credit = if gpu_layers > 0 {
            hardware
                .gpus
                .iter()
                .filter_map(|g| g.vram_total_bytes)
                .max()
                .filter(|vram| *vram >= 4 * 1024 * 1024 * 1024)
                .map_or(0, |vram| vram.min(model.size_bytes))
        } else {
            0
        };
        let available = hardware.available_memory_bytes + gpu_credit;
        if estimate.total_bytes > available {
            return Err(RuntimeError::InsufficientMemory {
                needed_mib: estimate.total_bytes / (1024 * 1024),
                available_mib: available / (1024 * 1024),
            });
        }
        let threads = settings
            .threads
            .or(hardware.physical_cores)
            .unwrap_or_else(|| (hardware.logical_cpus / 2).max(1))
            .max(1);

        let plan = LaunchPlan {
            binary: binary_path.clone(),
            model_path: PathBuf::from(&model.path),
            model_alias: model.id.clone(),
            port: process::free_port()?,
            context_size: settings.context_size,
            threads,
            gpu_layers,
            api_key: process::random_key(),
        };
        self.logs.clear();
        let command = (self.command_factory)(&plan);
        let child = process::spawn(command, self.adapter.as_ref(), &self.logs)?;
        let pid = child.id();
        if let Some(pid) = pid {
            self.write_pid(PidRecord {
                pid,
                exe: binary.path.clone(),
                port: plan.port,
            })
            .await;
        }
        self.state.send_replace(ServerState {
            phase: ServerPhase::Starting,
            model_id: Some(model.id.clone()),
            port: Some(plan.port),
            pid,
            started_at_ms: None,
            context_size: Some(plan.context_size),
            gpu_layers: Some(gpu_layers),
            threads: Some(threads),
            memory_bytes: None,
            error: None,
        });
        self.events.info(
            EventCategory::Runtime,
            format!(
                "Loading {} (context {}, {} GPU layers, {} threads, ~{} MiB)",
                model.display_name,
                plan.context_size,
                if gpu_layers >= ALL_LAYERS {
                    "all".to_owned()
                } else {
                    gpu_layers.to_string()
                },
                threads,
                estimate.total_bytes / (1024 * 1024)
            ),
        );

        let stop = CancellationToken::new();
        let timeout = Duration::from_secs(u64::from(settings.startup_timeout_secs));
        let task = tokio::spawn(Arc::clone(self).supervise(
            child,
            plan,
            model.display_name,
            stop.clone(),
            timeout,
        ));
        *supervisor = Some(Supervisor { stop, task });
        Ok(())
    }

    /// Stops the server if it runs. Safe to call at any time.
    pub async fn stop(&self) {
        let mut supervisor = self.supervisor.lock().await;
        if let Some(current) = supervisor.take() {
            self.stop_supervisor(current).await;
        }
    }

    async fn stop_supervisor(&self, supervisor: Supervisor) {
        if !supervisor.task.is_finished() {
            self.state.send_modify(|s| s.phase = ServerPhase::Stopping);
        }
        supervisor.stop.cancel();
        if tokio::time::timeout(STOP_TIMEOUT, supervisor.task)
            .await
            .is_err()
        {
            tracing::error!("llama-server supervisor did not stop in time");
        }
    }

    /// Stops the server and cancels downloads (application exit).
    pub async fn shutdown(&self) {
        self.downloads.cancel_all();
        self.stop().await;
    }

    async fn supervise(
        self: Arc<Self>,
        mut child: Child,
        plan: LaunchPlan,
        name: String,
        stop: CancellationToken,
        timeout: Duration,
    ) {
        let started = Instant::now();
        if let Err(err) = process::wait_ready(
            &self.client,
            plan.port,
            &mut child,
            timeout,
            &stop,
            &self.logs,
        )
        .await
        {
            terminate(&mut child).await;
            self.remove_pid().await;
            if matches!(err, RuntimeError::Cancelled) {
                self.state.send_replace(ServerState::stopped());
                self.events
                    .info(EventCategory::Runtime, format!("Loading {name} cancelled"));
            } else {
                self.events.error(
                    EventCategory::Runtime,
                    format!("{name} failed to start: {err}"),
                );
                self.state.send_modify(|s| {
                    s.phase = ServerPhase::Failed;
                    s.error = Some(err.to_string());
                    s.pid = None;
                });
            }
            return;
        }

        if !process::api_key_enforced(&self.client, plan.port).await {
            self.events.warning(
                EventCategory::Security,
                "The llama.cpp server accepted a request without its API key; other local programs could use it",
            );
        }
        let provider = match self.runtime_provider(&plan) {
            Ok(provider) => provider,
            Err(err) => {
                terminate(&mut child).await;
                self.remove_pid().await;
                self.state.send_modify(|s| {
                    s.phase = ServerPhase::Failed;
                    s.error = Some(err.to_string());
                    s.pid = None;
                });
                return;
            }
        };
        self.provider.send_replace(Some(provider));
        self.state.send_modify(|s| {
            s.phase = ServerPhase::Running;
            s.started_at_ms = Some(now_ms());
        });
        self.events.info(
            EventCategory::Runtime,
            format!("{name} is ready ({:.1} s)", started.elapsed().as_secs_f64()),
        );

        let exit = tokio::select! {
            status = child.wait() => Some(status),
            () = stop.cancelled() => None,
        };
        self.provider.send_replace(None);
        match exit {
            None => {
                terminate(&mut child).await;
                self.state.send_replace(ServerState::stopped());
                self.events
                    .info(EventCategory::Runtime, format!("{name} stopped"));
            }
            Some(status) => {
                let status = status.map_or_else(|err| err.to_string(), |s| s.to_string());
                let error = format!("llama-server exited unexpectedly ({status})");
                self.events
                    .error(EventCategory::Runtime, format!("{name}: {error}"));
                self.state.send_modify(|s| {
                    s.phase = ServerPhase::Failed;
                    s.error = Some(error);
                    s.pid = None;
                });
            }
        }
        self.remove_pid().await;
    }

    fn runtime_provider(&self, plan: &LaunchPlan) -> Result<Arc<dyn AiProvider>> {
        let descriptor = ProviderDescriptor {
            id: ProviderId::new(RUNTIME_PROVIDER_ID),
            display_name: "JARVIS runtime".to_owned(),
            kind: ProviderKind::Embedded,
            locality: DataLocality::OnDevice,
        };
        let base = url::Url::parse(&format!("http://127.0.0.1:{}/v1", plan.port))
            .map_err(|err| RuntimeError::Internal(format!("runtime URL: {err}")))?;
        Ok(Arc::new(
            OpenAiCompatibleProvider::new(descriptor, base, Arc::clone(&self.ai_http))
                .with_api_key(plan.api_key.clone()),
        ))
    }

    // ---- Orphan handling -------------------------------------------------------

    async fn write_pid(&self, record: PidRecord) {
        if let Ok(json) = serde_json::to_vec(&record)
            && let Err(err) = library::write_atomic(&self.runtime_dir.join(PID_FILE), &json).await
        {
            tracing::warn!("cannot record the server pid: {err}");
        }
    }

    async fn remove_pid(&self) {
        let _ = tokio::fs::remove_file(self.runtime_dir.join(PID_FILE)).await;
    }

    /// Kills a server left behind by a crashed session, but only if the pid
    /// still belongs to the recorded executable.
    async fn cleanup_orphan(&self) {
        let path = self.runtime_dir.join(PID_FILE);
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            return;
        };
        if let Ok(record) = serde_json::from_str::<PidRecord>(&text) {
            let killed = tokio::task::spawn_blocking(move || {
                jarvis_platform::kill_if_executable(record.pid, Path::new(&record.exe))
            })
            .await
            .unwrap_or(false);
            if killed {
                self.events.warning(
                    EventCategory::Runtime,
                    "Stopped a llama.cpp server left running by a previous session",
                );
            }
        }
        let _ = tokio::fs::remove_file(&path).await;
    }
}

async fn load_install_record(runtime_dir: &Path) -> Option<RuntimeBinary> {
    let text = tokio::fs::read_to_string(runtime_dir.join(INSTALL_FILE))
        .await
        .ok()?;
    match serde_json::from_str::<RuntimeBinary>(&text) {
        Ok(binary) if Path::new(&binary.path).is_file() => Some(binary),
        Ok(binary) => {
            tracing::warn!("installed runtime is missing: {}", binary.path);
            None
        }
        Err(err) => {
            tracing::warn!("runtime install record is invalid: {err}");
            None
        }
    }
}

/// Name of the first library that was added, removed or changed since the
/// binary was recorded.
fn changed_library(recorded: &[FileDigest], current: &[FileDigest]) -> Option<String> {
    current
        .iter()
        .find(|f| !recorded.contains(f))
        .or_else(|| recorded.iter().find(|f| !current.contains(f)))
        .map(|f| f.name.clone())
}

/// Graceful stop where the OS supports it, then a hard kill.
async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        let asked = tokio::task::spawn_blocking(move || jarvis_platform::request_termination(pid))
            .await
            .unwrap_or(false);
        if asked
            && tokio::time::timeout(GRACEFUL_STOP, child.wait())
                .await
                .is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|err| RuntimeError::io("marking llama-server executable", err))?;
    }
    let _ = path;
    Ok(())
}

/// GPU layers for the launch: explicit settings win; `auto` offloads
/// everything when the largest GPU's memory fits the model, a proportional
/// share otherwise, and everything when the memory is unknown (llama.cpp
/// reports an allocation failure, which surfaces as a start error).
pub fn gpu_layers(
    settings: &RuntimeSettings,
    backend: RuntimeBackend,
    hardware: &HardwareInfo,
    model: &LocalModel,
) -> u32 {
    if !backend.uses_gpu() {
        return 0;
    }
    match settings.gpu_offload {
        GpuOffload::Off => 0,
        GpuOffload::All => ALL_LAYERS,
        GpuOffload::Custom => settings.gpu_layers,
        GpuOffload::Auto => {
            let vram = hardware
                .gpus
                .iter()
                .filter_map(|g| g.vram_total_bytes)
                .max();
            match vram {
                None => ALL_LAYERS,
                Some(vram) if vram as f64 >= model.size_bytes as f64 * 1.2 => ALL_LAYERS,
                Some(vram) => {
                    let layers = f64::from(model.gguf.block_count.unwrap_or(32));
                    let share = (vram as f64 * 0.8) / model.size_bytes.max(1) as f64;
                    (layers * share).floor().clamp(0.0, layers) as u32
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GgufInfo;
    use jarvis_platform::{CpuFeatures, GpuInfo, GpuVendor};

    fn hardware(vram: Option<u64>) -> HardwareInfo {
        HardwareInfo {
            cpu_brand: None,
            physical_cores: Some(4),
            logical_cpus: 8,
            cpu_features: CpuFeatures::default(),
            total_memory_bytes: 16 << 30,
            available_memory_bytes: 8 << 30,
            gpus: vram
                .map(|v| GpuInfo {
                    name: "GPU".into(),
                    vendor: GpuVendor::Amd,
                    vram_total_bytes: Some(v),
                    vram_free_bytes: None,
                })
                .into_iter()
                .collect(),
            vulkan_available: true,
            nvidia_driver: None,
        }
    }

    fn model(size: u64) -> LocalModel {
        LocalModel {
            id: "m".into(),
            display_name: "M".into(),
            path: "m.gguf".into(),
            size_bytes: size,
            sha256: None,
            source: ModelSource::Imported,
            gguf: GgufInfo {
                block_count: Some(24),
                ..GgufInfo::default()
            },
            added_at_ms: 0,
            managed: false,
            available: true,
        }
    }

    #[test]
    fn library_changes_are_named() {
        let digest = |name: &str, sha: &str| FileDigest {
            name: name.into(),
            sha256: sha.into(),
        };
        let recorded = [digest("a.dll", "1"), digest("b.dll", "2")];
        assert_eq!(changed_library(&recorded, &recorded), None);
        let changed = [digest("a.dll", "1"), digest("b.dll", "3")];
        assert_eq!(
            changed_library(&recorded, &changed).as_deref(),
            Some("b.dll")
        );
        let added = [
            digest("a.dll", "1"),
            digest("b.dll", "2"),
            digest("c.dll", "4"),
        ];
        assert_eq!(changed_library(&recorded, &added).as_deref(), Some("c.dll"));
        let removed = [digest("a.dll", "1")];
        assert_eq!(
            changed_library(&recorded, &removed).as_deref(),
            Some("b.dll")
        );
    }

    #[test]
    fn gpu_layers_follow_settings_and_memory() {
        let auto = RuntimeSettings::default();
        let gib = 1u64 << 30;
        assert_eq!(
            gpu_layers(
                &auto,
                RuntimeBackend::Cpu,
                &hardware(Some(8 * gib)),
                &model(gib)
            ),
            0
        );
        assert_eq!(
            gpu_layers(
                &auto,
                RuntimeBackend::Vulkan,
                &hardware(Some(8 * gib)),
                &model(gib)
            ),
            ALL_LAYERS
        );
        assert_eq!(
            gpu_layers(&auto, RuntimeBackend::Vulkan, &hardware(None), &model(gib)),
            ALL_LAYERS
        );
        // 2 GiB of VRAM for a 4 GiB model: 24 * 0.4 = 9 layers.
        assert_eq!(
            gpu_layers(
                &auto,
                RuntimeBackend::Vulkan,
                &hardware(Some(2 * gib)),
                &model(4 * gib)
            ),
            9
        );

        let off = RuntimeSettings {
            gpu_offload: GpuOffload::Off,
            ..RuntimeSettings::default()
        };
        assert_eq!(
            gpu_layers(
                &off,
                RuntimeBackend::Vulkan,
                &hardware(Some(8 * gib)),
                &model(gib)
            ),
            0
        );
        let custom = RuntimeSettings {
            gpu_offload: GpuOffload::Custom,
            gpu_layers: 12,
            ..RuntimeSettings::default()
        };
        assert_eq!(
            gpu_layers(&custom, RuntimeBackend::Cuda, &hardware(None), &model(gib)),
            12
        );
    }
}
