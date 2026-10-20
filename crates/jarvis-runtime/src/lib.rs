//! Embedded local inference for JARVIS.
//!
//! JARVIS runs models without Ollama or LM Studio by supervising an official
//! llama.cpp `llama-server` process:
//!
//! - [`installer`]: finds official llama.cpp builds on GitHub, installs one
//!   only after explicit approval, verifies its SHA-256 and extracts it safely;
//! - [`library`] and [`gguf`]: GGUF model library with header validation;
//! - [`catalog`] and [`download`]: verified model downloads from Hugging Face;
//! - [`process`] and [`RuntimeManager`]: launching on `127.0.0.1` with a random
//!   API key, readiness checks, crash detection and safe termination.
//!
//! Two servers can run side by side, one per [`ServerRole`]: a chat model and
//! an embedding model (`--embedding`) for semantic memory search. While a
//! model is served, [`RuntimeManager::provider`] exposes it as an
//! OpenAI-compatible [`jarvis_ai::AiProvider`].

pub mod catalog;
pub mod download;
mod error;
pub mod gguf;
pub mod installer;
pub mod library;
mod manager;
pub mod process;
#[cfg(feature = "test-support")]
pub mod testing;
mod types;

pub use error::{Result, RuntimeError};
pub use manager::{
    EMBEDDING_CONTEXT, RUNTIME_EMBEDDINGS_PROVIDER_ID, RUNTIME_PROVIDER_ID, RuntimeManager,
    RuntimeOptions, gpu_layers,
};
pub use types::{
    BinarySource, CuratedModel, DownloadKind, DownloadPhase, DownloadStatus, FileDigest, GgufInfo,
    GpuOffload, LocalModel, MemoryEstimate, ModelDownloadRequest, ModelSource, PoolingType,
    RuntimeBackend, RuntimeBinary, RuntimeRelease, RuntimeSettings, RuntimeStatus, ServerPhase,
    ServerRole, ServerState,
};
