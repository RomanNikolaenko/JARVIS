//! Provider-agnostic AI layer.
//!
//! The rest of JARVIS talks to models exclusively through the [`AiProvider`]
//! trait. Concrete providers (Ollama, OpenAI-compatible servers, the embedded
//! llama.cpp runtime, cloud APIs) are added as separate implementations and
//! registered in a [`ProviderRegistry`].
//!
//! Phase 1 ships only the [`EchoProvider`], a clearly labelled diagnostic
//! provider used to exercise streaming and cancellation end to end.

mod echo;
mod error;
mod provider;
mod registry;
mod types;

pub use echo::EchoProvider;
pub use error::AiError;
pub use provider::{AiProvider, ChatStream};
pub use registry::ProviderRegistry;
pub use types::{
    ChatChunk, ChatMessage, ChatRequest, ChatResponse, ChatRole, DataLocality, EmbeddingRequest,
    EmbeddingResponse, FinishReason, GenerationOptions, ModelCapabilities, ModelInfo,
    ProviderDescriptor, ProviderId, ProviderKind, TokenUsage,
};
