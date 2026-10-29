//! Orchestrates the local runtime: installation, model library, downloads
//! and the supervised `llama-server` processes — one for chat and one for
//! embeddings, each in its own [`Slot`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use jarvis_ai::{
    AiProvider, DataLocality, HttpClient, ModelCapabilities, OpenAiCompatibleProvider,
    ProviderDescriptor, ProviderId, ProviderKind,
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
use crate::process::{self, CommandFactory, EmbeddingOptions, LaunchPlan, LogTail};
use crate::types::{
    BinarySource, CuratedModel, DownloadKind, DownloadPhase, FileDigest, GgufInfo, GpuOffload,
    LocalModel, ModelDownloadRequest, ModelSource, PoolingType, ProjectorFile, RuntimeBackend,
    RuntimeBinary, RuntimeRelease, RuntimeSettings, RuntimeStatus, ServerPhase, ServerRole,
    ServerState,
};

/// Provider id of the embedded chat server in the AI registry.
pub const RUNTIME_PROVIDER_ID: &str = "jarvis-runtime";
/// Provider id of the embedded embedding server in the AI registry.
pub const RUNTIME_EMBEDDINGS_PROVIDER_ID: &str = "jarvis-runtime-embeddings";
/// Context of the embedding server (at most the model's training context).
/// It is also the batch and micro-batch size, because llama-server rejects
/// an input that does not fit into one micro-batch. JARVIS embeds at most
/// 2 000 characters per text, which stays below this for SentencePiece and
/// WordPiece tokenizers (at most one token per character).
pub const EMBEDDING_CONTEXT: u32 = 2048;
const INSTALL_FILE: &str = "install.json";
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

/// State of one supervised server. Every role gets the same treatment:
/// verified binary, loopback-only port, per-launch API key, pid file,
/// readiness and crash detection.
struct Slot {
    role: ServerRole,
    state: watch::Sender<ServerState>,
    provider: watch::Sender<Option<Arc<dyn AiProvider>>>,
    logs: LogTail,
    supervisor: tokio::sync::Mutex<Option<Supervisor>>,
}

impl Slot {
    fn new(role: ServerRole) -> Self {
        Self {
            role,
            state: watch::Sender::new(ServerState::stopped()),
            provider: watch::Sender::new(None),
            logs: LogTail::default(),
            supervisor: tokio::sync::Mutex::new(None),
        }
    }

    fn state(&self) -> ServerState {
        self.state.borrow().clone()
    }

    fn is_active(&self) -> bool {
        matches!(
            self.state.borrow().phase,
            ServerPhase::Starting | ServerPhase::Running
        )
    }

    /// Records the server's pid for orphan cleanup by the next session.
    fn pid_file(&self) -> &'static str {
        match self.role {
            ServerRole::Chat => "server.pid",
            ServerRole::Embeddings => "embedding-server.pid",
        }
    }

    /// Suffix of event messages ("Loading X for embeddings").
    fn purpose(&self) -> &'static str {
        match self.role {
            ServerRole::Chat => "",
            ServerRole::Embeddings => " for embeddings",
        }
    }

    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: ProviderId::new(self.role.provider_id()),
            display_name: match self.role {
                ServerRole::Chat => "JARVIS runtime",
                ServerRole::Embeddings => "JARVIS runtime (embeddings)",
            }
            .to_owned(),
            kind: ProviderKind::Embedded,
            locality: DataLocality::OnDevice,
        }
    }

    /// What the served model can do: a server started with `--embedding`
    /// only embeds, a chat server cannot embed.
    fn capabilities(&self) -> ModelCapabilities {
        let state = self.state.borrow();
        match self.role {
            ServerRole::Chat => ModelCapabilities {
                chat: true,
                streaming: true,
                vision: state.vision,
                tool_calling: state.tool_calling,
                ..ModelCapabilities::default()
            },
            ServerRole::Embeddings => ModelCapabilities {
                embeddings: true,
                ..ModelCapabilities::default()
            },
        }
    }

    /// Marks the slot failed after a start or a run went wrong.
    fn fail(&self, error: String) {
        self.state.send_modify(|s| {
            s.phase = ServerPhase::Failed;
            s.error = Some(error);
            s.pid = None;
        });
    }
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
    chat: Slot,
    embeddings: Slot,
}

impl std::fmt::Debug for RuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeManager")
            .field("runtime_dir", &self.runtime_dir)
            .field("chat", &*self.chat.state.borrow())
            .field("embeddings", &*self.embeddings.state.borrow())
            .finish_non_exhaustive()
    }
}

impl RuntimeManager {
    /// Loads the installation record and model library and removes servers
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
            chat: Slot::new(ServerRole::Chat),
            embeddings: Slot::new(ServerRole::Embeddings),
        });
        for role in ServerRole::ALL {
            manager.cleanup_orphan(manager.slot(role)).await;
        }
        Ok(manager)
    }

    fn slot(&self, role: ServerRole) -> &Slot {
        match role {
            ServerRole::Chat => &self.chat,
            ServerRole::Embeddings => &self.embeddings,
        }
    }

    fn slots(&self) -> [&Slot; 2] {
        [&self.chat, &self.embeddings]
    }

    // ---- Observation --------------------------------------------------

    pub fn subscribe_provider(
        &self,
        role: ServerRole,
    ) -> watch::Receiver<Option<Arc<dyn AiProvider>>> {
        self.slot(role).provider.subscribe()
    }

    /// The provider of a role while its model is being served.
    pub fn provider(&self, role: ServerRole) -> Option<Arc<dyn AiProvider>> {
        self.slot(role).provider.borrow().clone()
    }

    /// Providers of every role that currently serves a model (chat first).
    pub fn providers(&self) -> Vec<Arc<dyn AiProvider>> {
        self.slots()
            .iter()
            .filter_map(|slot| slot.provider.borrow().clone())
            .collect()
    }

    pub fn subscribe_state(&self, role: ServerRole) -> watch::Receiver<ServerState> {
        self.slot(role).state.subscribe()
    }

    pub fn server_state(&self, role: ServerRole) -> ServerState {
        self.slot(role).state()
    }

    pub fn binary(&self) -> Option<RuntimeBinary> {
        self.binary
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub async fn status(&self) -> RuntimeStatus {
        let (server, embedding_server) =
            tokio::join!(sampled_state(&self.chat), sampled_state(&self.embeddings));
        RuntimeStatus {
            binary: self.binary(),
            server,
            embedding_server,
            models: self.library.list(),
            downloads: self.downloads.snapshot(),
            models_dir: self.library.dir().display().to_string(),
            log_tail: self.chat.logs.last(LOG_TAIL_IN_STATUS),
            embedding_log_tail: self.embeddings.logs.last(LOG_TAIL_IN_STATUS),
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

    /// Waits until the role's server is no longer starting or stopping.
    pub async fn wait_settled(&self, role: ServerRole, timeout: Duration) -> ServerState {
        let slot = self.slot(role);
        let mut receiver = slot.state.subscribe();
        let _ = tokio::time::timeout(
            timeout,
            receiver
                .wait_for(|s| !matches!(s.phase, ServerPhase::Starting | ServerPhase::Stopping)),
        )
        .await;
        slot.state()
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
            && self.slots().iter().any(|slot| slot.is_active())
        {
            return Err(RuntimeError::InvalidInput(
                "this build is in use; stop the local models first".into(),
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

    /// Attaches a vision projector (`mmproj` GGUF) the user picked to a
    /// model, so the model can see images. Takes effect on the next start.
    pub async fn attach_projector(&self, model_id: &str, path: &Path) -> Result<LocalModel> {
        let path = library::canonical(path)
            .await
            .map_err(|err| RuntimeError::io(format!("opening {}", path.display()), err))?;
        let header = path.clone();
        let info = tokio::task::spawn_blocking(move || gguf::read_gguf_info(&header))
            .await
            .map_err(|err| RuntimeError::Internal(err.to_string()))??;
        if info.architecture.as_deref() != Some("clip") {
            return Err(RuntimeError::InvalidModel(
                "this is not a vision projector (an mmproj GGUF file)".into(),
            ));
        }
        let size_bytes = tokio::fs::metadata(&path)
            .await
            .map_err(|err| RuntimeError::io("reading the projector", err))?
            .len();
        let model = self
            .library
            .set_projector(
                model_id,
                Some(ProjectorFile {
                    path: path.display().to_string(),
                    size_bytes,
                    sha256: None,
                }),
            )
            .await?;
        self.events.info(
            EventCategory::Runtime,
            format!("Vision projector added to {}", model.display_name),
        );
        Ok(model)
    }

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
        let projector = model
            .projector
            .as_ref()
            .map(|p| ResolvedFile::from_curated_projector(&model, p))
            .transpose()?;
        self.start_download(resolved, model.display_name, projector)
    }

    pub async fn download_model(self: &Arc<Self>, request: ModelDownloadRequest) -> Result<String> {
        let resolved = catalog::resolve(&self.client, &request).await?;
        let label = format!("{} / {}", resolved.repo, resolved.file);
        self.start_download(resolved, label, None)
    }

    fn start_download(
        self: &Arc<Self>,
        resolved: ResolvedFile,
        label: String,
        projector: Option<ResolvedFile>,
    ) -> Result<String> {
        download::check_source(&resolved.url)?;
        if let Some(projector) = &projector {
            download::check_source(&projector.url)?;
        }
        let dest = self.library.download_path(resolved.file_name());
        let already = self.library.list().into_iter().any(|m| {
            Path::new(&m.path) == dest.as_path()
                && m.available
                && m.sha256.as_deref() == Some(&resolved.expected.sha256)
                && (projector.is_none() || m.projector.is_some())
        });
        if already {
            return Err(RuntimeError::InvalidInput(format!(
                "{label} is already downloaded"
            )));
        }
        let handle = self.downloads.begin_model(label.clone(), dest.clone())?;
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
                let model = this
                    .library
                    .add_downloaded(dest.clone(), source, resolved.expected.sha256.clone())
                    .await?;
                let Some(projector) = projector else {
                    return Ok(model);
                };
                // Generic projector names (mmproj-model-f16.gguf) would
                // collide between models.
                let projector_dest =
                    this.library
                        .download_path(&format!("{}.{}", model.id, projector.file_name()));
                download::fetch_verified(
                    &this.client,
                    &projector.url,
                    &projector_dest,
                    &projector.expected,
                    &handle,
                )
                .await?;
                this.library
                    .set_projector(
                        &model.id,
                        Some(ProjectorFile {
                            path: projector_dest.display().to_string(),
                            size_bytes: projector.expected.size.unwrap_or_default(),
                            sha256: Some(projector.expected.sha256.clone()),
                        }),
                    )
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
        let in_use = self
            .slots()
            .iter()
            .any(|slot| slot.is_active() && slot.state().model_id.as_deref() == Some(id));
        if in_use {
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

    /// Starts serving `model_id` in the role's server (restarting it if
    /// another model runs there). Returns once the process is spawned;
    /// readiness is reported via the state.
    pub async fn start(
        self: &Arc<Self>,
        role: ServerRole,
        model_id: &str,
        settings: &RuntimeSettings,
    ) -> Result<()> {
        let slot = self.slot(role);
        let mut supervisor = slot.supervisor.lock().await;
        if let Some(previous) = supervisor.take() {
            stop_supervisor(slot, previous).await;
        }

        let binary = self.verified_binary().await?;
        let model = self.library.get(model_id).ok_or_else(|| {
            RuntimeError::NotFound(format!("model \"{model_id}\" is not in the library"))
        })?;
        if !model.available {
            return Err(RuntimeError::NotFound(format!(
                "the file of \"{}\" is missing or changed: {}",
                model.display_name, model.path
            )));
        }
        // Only a chat server loads the projector.
        let projector = match (&model.projector, role) {
            (Some(projector), ServerRole::Chat) => {
                let present = tokio::fs::metadata(&projector.path)
                    .await
                    .is_ok_and(|m| m.is_file() && m.len() == projector.size_bytes);
                if !present {
                    return Err(RuntimeError::NotFound(format!(
                        "the vision projector of \"{}\" is missing or changed: {}",
                        model.display_name, projector.path
                    )));
                }
                Some(projector.clone())
            }
            _ => None,
        };
        let (context_size, embeddings) = match role {
            ServerRole::Chat => (settings.context_size, None),
            ServerRole::Embeddings => (
                embedding_context(&model.gguf),
                Some(EmbeddingOptions {
                    pooling: self.embedding_pooling(&model).await?,
                }),
            ),
        };

        let hardware = Self::hardware().await;
        let gpu_layers = gpu_layers(settings, binary.backend, &hardware, &model);
        let mut estimate = gguf::estimate_memory(model.size_bytes, &model.gguf, context_size);
        if let Some(projector) = &projector {
            estimate.total_bytes += projector.size_bytes;
        }
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
            binary: PathBuf::from(&binary.path),
            model_path: PathBuf::from(&model.path),
            model_alias: model.id.clone(),
            port: process::free_port()?,
            context_size,
            threads,
            gpu_layers,
            api_key: process::random_key(),
            embeddings,
            projector: projector.as_ref().map(|p| PathBuf::from(&p.path)),
            native_tools: role == ServerRole::Chat
                && settings.native_tool_models.contains(&model.id),
        };
        slot.logs.clear();
        let command = (self.command_factory)(&plan);
        let child = process::spawn(command, self.adapter.as_ref(), &slot.logs)?;
        let pid = child.id();
        if let Some(pid) = pid {
            self.write_pid(
                slot,
                PidRecord {
                    pid,
                    exe: binary.path.clone(),
                    port: plan.port,
                },
            )
            .await;
        }
        slot.state.send_replace(ServerState {
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
            vision: projector.is_some(),
            tool_calling: plan.native_tools,
        });
        self.events.info(
            EventCategory::Runtime,
            format!(
                "Loading {}{} (context {}, {} GPU layers, {} threads, ~{} MiB)",
                model.display_name,
                slot.purpose(),
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
            role,
            child,
            plan,
            model.display_name,
            stop.clone(),
            timeout,
        ));
        *supervisor = Some(Supervisor { stop, task });
        Ok(())
    }

    /// The installed binary, after checking that neither the executable nor
    /// a library next to it changed since installation.
    async fn verified_binary(&self) -> Result<RuntimeBinary> {
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
        Ok(binary)
    }

    /// Pooling for an embedding server, from the model file itself (library
    /// entries made before pooling was recorded do not have it).
    async fn embedding_pooling(&self, model: &LocalModel) -> Result<PoolingType> {
        let path = PathBuf::from(&model.path);
        let info = tokio::task::spawn_blocking(move || gguf::read_gguf_info(&path))
            .await
            .map_err(|err| RuntimeError::Internal(err.to_string()))??;
        match info.pooling {
            Some(PoolingType::Rank) => Err(RuntimeError::InvalidInput(format!(
                "{} is a reranking model and produces no embeddings",
                model.display_name
            ))),
            Some(pooling @ (PoolingType::Mean | PoolingType::Cls | PoolingType::Last)) => {
                Ok(pooling)
            }
            // The OpenAI-compatible endpoint needs pooled vectors.
            Some(PoolingType::None) | None => {
                self.events.warning(
                    EventCategory::Runtime,
                    format!(
                        "{} declares no pooling type, so mean pooling is used; models that are not embedding models give poor search results",
                        model.display_name
                    ),
                );
                Ok(PoolingType::Mean)
            }
        }
    }

    /// Stops the role's server if it runs. Safe to call at any time.
    pub async fn stop(&self, role: ServerRole) {
        let slot = self.slot(role);
        let mut supervisor = slot.supervisor.lock().await;
        if let Some(current) = supervisor.take() {
            stop_supervisor(slot, current).await;
        }
    }

    /// Stops all servers and cancels downloads (application exit).
    pub async fn shutdown(&self) {
        self.downloads.cancel_all();
        tokio::join!(
            self.stop(ServerRole::Chat),
            self.stop(ServerRole::Embeddings)
        );
    }

    async fn supervise(
        self: Arc<Self>,
        role: ServerRole,
        mut child: Child,
        plan: LaunchPlan,
        name: String,
        stop: CancellationToken,
        timeout: Duration,
    ) {
        let slot = self.slot(role);
        let started = Instant::now();
        if let Err(err) = process::wait_ready(
            &self.client,
            plan.port,
            &mut child,
            timeout,
            &stop,
            &slot.logs,
        )
        .await
        {
            terminate(&mut child).await;
            self.remove_pid(slot).await;
            if matches!(err, RuntimeError::Cancelled) {
                slot.state.send_replace(ServerState::stopped());
                self.events.info(
                    EventCategory::Runtime,
                    format!("Loading {name}{} cancelled", slot.purpose()),
                );
            } else {
                self.events.error(
                    EventCategory::Runtime,
                    format!("{name} failed to start{}: {err}", slot.purpose()),
                );
                slot.fail(err.to_string());
            }
            return;
        }

        if !process::api_key_enforced(&self.client, plan.port).await {
            self.events.warning(
                EventCategory::Security,
                "The llama.cpp server accepted a request without its API key; other local programs could use it",
            );
        }
        let provider = match self.runtime_provider(slot, &plan) {
            Ok(provider) => provider,
            Err(err) => {
                terminate(&mut child).await;
                self.remove_pid(slot).await;
                slot.fail(err.to_string());
                return;
            }
        };
        slot.provider.send_replace(Some(provider));
        slot.state.send_modify(|s| {
            s.phase = ServerPhase::Running;
            s.started_at_ms = Some(now_ms());
        });
        self.events.info(
            EventCategory::Runtime,
            format!(
                "{name} is ready{} ({:.1} s)",
                slot.purpose(),
                started.elapsed().as_secs_f64()
            ),
        );

        let exit = tokio::select! {
            status = child.wait() => Some(status),
            () = stop.cancelled() => None,
        };
        slot.provider.send_replace(None);
        // The pid file goes before the state changes, so observers of the
        // state never see a stale record.
        match exit {
            None => {
                terminate(&mut child).await;
                self.remove_pid(slot).await;
                slot.state.send_replace(ServerState::stopped());
                self.events.info(
                    EventCategory::Runtime,
                    format!("{name} stopped{}", slot.purpose()),
                );
            }
            Some(status) => {
                self.remove_pid(slot).await;
                let status = status.map_or_else(|err| err.to_string(), |s| s.to_string());
                let error = format!("llama-server exited unexpectedly ({status})");
                self.events.error(
                    EventCategory::Runtime,
                    format!("{name}{}: {error}", slot.purpose()),
                );
                slot.fail(error);
            }
        }
    }

    fn runtime_provider(&self, slot: &Slot, plan: &LaunchPlan) -> Result<Arc<dyn AiProvider>> {
        let base = url::Url::parse(&format!("http://127.0.0.1:{}/v1", plan.port))
            .map_err(|err| RuntimeError::Internal(format!("runtime URL: {err}")))?;
        Ok(Arc::new(
            OpenAiCompatibleProvider::new(slot.descriptor(), base, Arc::clone(&self.ai_http))
                .with_api_key(plan.api_key.clone())
                .with_capabilities(slot.capabilities()),
        ))
    }

    // ---- Orphan handling -------------------------------------------------------

    async fn write_pid(&self, slot: &Slot, record: PidRecord) {
        let path = self.runtime_dir.join(slot.pid_file());
        if let Ok(json) = serde_json::to_vec(&record)
            && let Err(err) = library::write_atomic(&path, &json).await
        {
            tracing::warn!("cannot record the server pid: {err}");
        }
    }

    async fn remove_pid(&self, slot: &Slot) {
        let _ = tokio::fs::remove_file(self.runtime_dir.join(slot.pid_file())).await;
    }

    /// Kills a server left behind by a crashed session, but only if the pid
    /// still belongs to the recorded executable.
    async fn cleanup_orphan(&self, slot: &Slot) {
        let path = self.runtime_dir.join(slot.pid_file());
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
                    format!(
                        "Stopped a llama.cpp server{} left running by a previous session",
                        slot.purpose()
                    ),
                );
            }
        }
        let _ = tokio::fs::remove_file(&path).await;
    }
}

/// The slot's state with the server's current memory use.
async fn sampled_state(slot: &Slot) -> ServerState {
    let mut state = slot.state();
    if let Some(pid) = state.pid
        && matches!(state.phase, ServerPhase::Starting | ServerPhase::Running)
    {
        state.memory_bytes =
            tokio::task::spawn_blocking(move || jarvis_platform::process_memory(pid))
                .await
                .ok()
                .flatten();
    }
    state
}

async fn stop_supervisor(slot: &Slot, supervisor: Supervisor) {
    if !supervisor.task.is_finished() {
        slot.state.send_modify(|s| s.phase = ServerPhase::Stopping);
    }
    supervisor.stop.cancel();
    if tokio::time::timeout(STOP_TIMEOUT, supervisor.task)
        .await
        .is_err()
    {
        tracing::error!("llama-server supervisor did not stop in time");
    }
}

/// Context of the embedding server: [`EMBEDDING_CONTEXT`], or less if the
/// model was trained for less (BERT-like models have no positions beyond).
fn embedding_context(info: &GgufInfo) -> u32 {
    match info.context_length {
        Some(trained) if trained > 0 => trained.min(EMBEDDING_CONTEXT),
        _ => EMBEDDING_CONTEXT,
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
            projector: None,
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
    fn embedding_context_stays_within_the_training_context() {
        let trained = |context_length| GgufInfo {
            context_length,
            ..GgufInfo::default()
        };
        // bge-m3 is trained for 8192 tokens; one batch of 2048 covers every text.
        assert_eq!(embedding_context(&trained(Some(8192))), EMBEDDING_CONTEXT);
        // multilingual-e5 has 512 positions; more would read past them.
        assert_eq!(embedding_context(&trained(Some(512))), 512);
        assert_eq!(embedding_context(&trained(None)), EMBEDDING_CONTEXT);
        assert_eq!(embedding_context(&trained(Some(0))), EMBEDDING_CONTEXT);
    }

    #[test]
    fn slots_describe_their_providers() {
        let chat = Slot::new(ServerRole::Chat);
        let embeddings = Slot::new(ServerRole::Embeddings);
        assert_eq!(chat.descriptor().id.as_str(), RUNTIME_PROVIDER_ID);
        assert_eq!(
            embeddings.descriptor().id.as_str(),
            RUNTIME_EMBEDDINGS_PROVIDER_ID
        );
        assert_eq!(embeddings.descriptor().locality, DataLocality::OnDevice);
        assert!(chat.capabilities().chat && !chat.capabilities().embeddings);
        assert!(embeddings.capabilities().embeddings && !embeddings.capabilities().chat);
        assert_ne!(chat.pid_file(), embeddings.pid_file());
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
