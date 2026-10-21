//! Data types shared with the UI (exported to TypeScript).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Compute backend of a llama.cpp build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub enum RuntimeBackend {
    Cpu,
    Vulkan,
    Cuda,
    Other,
}

impl RuntimeBackend {
    pub fn uses_gpu(self) -> bool {
        matches!(self, Self::Vulkan | Self::Cuda | Self::Other)
    }
}

/// Where the installed `llama-server` binary came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export_to = "runtime.ts")]
pub enum BinarySource {
    /// Official llama.cpp release downloaded and verified by JARVIS.
    Release {
        tag: String,
        asset: String,
        url: String,
    },
    /// A binary the user selected.
    UserSelected,
}

/// The `llama-server` executable JARVIS runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct RuntimeBinary {
    pub path: String,
    /// First line of `llama-server --version`.
    pub version: String,
    pub backend: RuntimeBackend,
    pub source: BinarySource,
    /// SHA-256 of the executable, checked before every start.
    pub sha256: String,
    /// Shared libraries next to the executable, also checked before every
    /// start: the executable may be a thin launcher (Windows builds) and the
    /// loader picks libraries up from its directory.
    #[serde(default)]
    pub libraries: Vec<FileDigest>,
    pub installed_at_ms: u64,
}

/// SHA-256 of a file, by name within its directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct FileDigest {
    pub name: String,
    pub sha256: String,
}

/// A llama.cpp release asset offered for installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct RuntimeRelease {
    pub tag: String,
    pub published_at: String,
    pub backend: RuntimeBackend,
    pub asset_name: String,
    pub download_url: String,
    pub size_bytes: u64,
    /// SHA-256 published by GitHub for the asset.
    pub sha256: String,
}

/// How an embedding model turns token vectors into one vector per input
/// (`<arch>.pooling_type` in GGUF, `--pooling` of llama.cpp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub enum PoolingType {
    None,
    Mean,
    Cls,
    Last,
    /// Reranking models: they score pairs and produce no embeddings.
    Rank,
}

impl PoolingType {
    /// Value of the GGUF key (llama.cpp's `llama_pooling_type`).
    pub fn from_gguf(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::None,
            1 => Self::Mean,
            2 => Self::Cls,
            3 => Self::Last,
            4 => Self::Rank,
            _ => return None,
        })
    }

    /// Value of llama.cpp's `--pooling` option.
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Mean => "mean",
            Self::Cls => "cls",
            Self::Last => "last",
            Self::Rank => "rank",
        }
    }
}

/// Metadata read from a GGUF header.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct GgufInfo {
    pub architecture: Option<String>,
    pub name: Option<String>,
    /// e.g. "0.5B" (`general.size_label`).
    pub size_label: Option<String>,
    /// Quantization, e.g. "Q4_K_M".
    pub quantization: Option<String>,
    /// Maximum context the model was trained for.
    pub context_length: Option<u32>,
    pub block_count: Option<u32>,
    pub embedding_length: Option<u32>,
    pub head_count: Option<u32>,
    pub head_count_kv: Option<u32>,
    /// Declared by embedding models (sentence-transformers conversions).
    #[serde(default)]
    pub pooling: Option<PoolingType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export_to = "runtime.ts")]
pub enum ModelSource {
    /// Imported by the user; the file stays where it is.
    Imported,
    /// Downloaded from Hugging Face and verified.
    Downloaded {
        repo: String,
        file: String,
        revision: String,
    },
}

/// A GGUF model known to JARVIS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct LocalModel {
    pub id: String,
    pub display_name: String,
    pub path: String,
    pub size_bytes: u64,
    /// Verified SHA-256 (downloaded models only).
    pub sha256: Option<String>,
    pub source: ModelSource,
    pub gguf: GgufInfo,
    pub added_at_ms: u64,
    /// The file lives in JARVIS's models directory and may be deleted by JARVIS.
    pub managed: bool,
    /// The file exists and has the recorded size (computed when listing).
    #[serde(default)]
    pub available: bool,
    /// Vision projector (`mmproj`): with it the model can see images.
    #[serde(default)]
    pub projector: Option<ProjectorFile>,
}

/// A vision projector (`mmproj` GGUF) for a chat model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct ProjectorFile {
    pub path: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
    /// Verified SHA-256 (downloaded projectors only).
    pub sha256: Option<String>,
}

/// An additional file of a curated model (same repository and revision).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct CuratedFile {
    pub file: String,
    pub sha256: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
}

/// A model from the curated catalog: pinned repository revision and checksum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct CuratedModel {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub repo: String,
    pub file: String,
    pub revision: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub license: String,
    /// The server the model is meant for.
    pub role: ServerRole,
    /// Vision projector downloaded with the model.
    pub projector: Option<CuratedFile>,
}

/// A user-specified Hugging Face file to download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct ModelDownloadRequest {
    /// `owner/name`.
    pub repo: String,
    /// Path of the `.gguf` file in the repository.
    pub file: String,
    /// Branch, tag or commit; defaults to `main`.
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub enum DownloadKind {
    Model,
    Runtime,
    /// Voice engines, speech models and voices.
    Voice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub enum DownloadPhase {
    Downloading,
    Verifying,
    Installing,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadPhase {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct DownloadStatus {
    pub id: String,
    pub kind: DownloadKind,
    pub label: String,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: u64,
    pub phase: DownloadPhase,
    pub error: Option<String>,
    pub started_at_ms: u64,
}

/// What a managed server instance is for. Each role runs its own
/// `llama-server` process with its own port, API key and AI provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub enum ServerRole {
    /// Chat completions.
    Chat,
    /// Embedding vectors for semantic memory search (`--embedding`).
    Embeddings,
}

impl ServerRole {
    pub const ALL: [Self; 2] = [Self::Chat, Self::Embeddings];

    /// Id of the role's provider in the AI registry.
    pub fn provider_id(self) -> &'static str {
        match self {
            Self::Chat => crate::RUNTIME_PROVIDER_ID,
            Self::Embeddings => crate::RUNTIME_EMBEDDINGS_PROVIDER_ID,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub enum ServerPhase {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

/// State of the managed llama.cpp server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct ServerState {
    pub phase: ServerPhase,
    pub model_id: Option<String>,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub started_at_ms: Option<u64>,
    pub context_size: Option<u32>,
    pub gpu_layers: Option<u32>,
    pub threads: Option<u32>,
    /// Resident memory of the server process (sampled when status is read).
    pub memory_bytes: Option<u64>,
    pub error: Option<String>,
    /// Started with a vision projector: the model can see images.
    #[serde(default)]
    pub vision: bool,
}

impl ServerState {
    pub fn stopped() -> Self {
        Self {
            phase: ServerPhase::Stopped,
            model_id: None,
            port: None,
            pid: None,
            started_at_ms: None,
            context_size: None,
            gpu_layers: None,
            threads: None,
            memory_bytes: None,
            error: None,
            vision: false,
        }
    }
}

/// Everything the Runtime screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct RuntimeStatus {
    pub binary: Option<RuntimeBinary>,
    /// The chat model server.
    pub server: ServerState,
    /// The embedding model server.
    pub embedding_server: ServerState,
    pub models: Vec<LocalModel>,
    pub downloads: Vec<DownloadStatus>,
    pub models_dir: String,
    /// Last lines printed by the chat server (diagnostics).
    pub log_tail: Vec<String>,
    /// Last lines printed by the embedding server.
    pub embedding_log_tail: Vec<String>,
}

/// How many model layers are offloaded to the GPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub enum GpuOffload {
    /// Decide from the backend, GPU memory and model size.
    #[default]
    Auto,
    /// CPU only.
    Off,
    /// All layers.
    All,
    /// Exactly `gpuLayers` layers.
    Custom,
}

/// Inference settings of the local runtime (part of the user settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "runtime.ts")]
pub struct RuntimeSettings {
    /// Context window in tokens.
    pub context_size: u32,
    /// CPU threads; `None` = number of physical cores.
    pub threads: Option<u32>,
    pub gpu_offload: GpuOffload,
    /// Used when `gpuOffload` is `custom`.
    pub gpu_layers: u32,
    /// Chat model started automatically with JARVIS; `None` = never.
    pub autostart_model: Option<String>,
    /// Embedding model started automatically with JARVIS; `None` = never.
    pub autostart_embedding_model: Option<String>,
    /// Maximum time to load a model.
    pub startup_timeout_secs: u32,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            context_size: 4096,
            threads: None,
            gpu_offload: GpuOffload::Auto,
            gpu_layers: 0,
            autostart_model: None,
            autostart_embedding_model: None,
            startup_timeout_secs: 180,
        }
    }
}

impl RuntimeSettings {
    /// Range checks; returns human-readable problems.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if !(512..=262_144).contains(&self.context_size) {
            problems.push("runtime.contextSize must be between 512 and 262144".to_owned());
        }
        if let Some(threads) = self.threads
            && !(1..=256).contains(&threads)
        {
            problems.push("runtime.threads must be between 1 and 256".to_owned());
        }
        if self.gpu_layers > 1000 {
            problems.push("runtime.gpuLayers must be at most 1000".to_owned());
        }
        if !(10..=1800).contains(&self.startup_timeout_secs) {
            problems.push("runtime.startupTimeoutSecs must be between 10 and 1800".to_owned());
        }
        if let Some(model) = &self.autostart_model
            && !crate::library::is_valid_model_id(model)
        {
            problems.push("runtime.autostartModel has an invalid format".to_owned());
        }
        if let Some(model) = &self.autostart_embedding_model
            && !crate::library::is_valid_model_id(model)
        {
            problems.push("runtime.autostartEmbeddingModel has an invalid format".to_owned());
        }
        problems
    }
}

/// Estimated memory needed to run a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "runtime.ts")]
pub struct MemoryEstimate {
    pub model_bytes: u64,
    pub kv_cache_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
}
