//! Provider-agnostic AI layer.
//!
//! The rest of JARVIS talks to models exclusively through the [`AiProvider`]
//! trait. Concrete providers are registered in a [`ProviderRegistry`]:
//!
//! - [`OllamaProvider`]: Ollama native API;
//! - [`OpenAiCompatibleProvider`]: LM Studio, llama.cpp server, vLLM, ...;
//! - [`EchoProvider`]: clearly labelled diagnostic provider without a model.
//!
//! User-configured endpoints ([`ProviderConfig`]) are turned into providers by
//! a [`ProviderFactory`]; their privacy class ([`DataLocality`]) is derived
//! from the endpoint URL, never from user input.

mod anthropic;
mod config;
mod echo;
mod error;
mod factory;
mod gemini;
mod http;
mod ollama;
mod openai;
mod provider;
mod registry;
mod types;

pub use anthropic::AnthropicProvider;
pub use config::{
    ProviderBackend, ProviderConfig, classify_locality, is_valid_provider_id, parse_base_url,
};
pub use echo::EchoProvider;
pub use error::AiError;
pub use factory::{HttpProviderFactory, ProviderFactory};
pub use gemini::GeminiProvider;
pub use http::HttpClient;
pub use ollama::OllamaProvider;
pub use openai::OpenAiCompatibleProvider;
pub use provider::{AiProvider, ChatStream};
pub use registry::ProviderRegistry;
pub use types::{
    ChatChunk, ChatMessage, ChatRequest, ChatResponse, ChatRole, DataLocality, EmbeddingRequest,
    EmbeddingResponse, FinishReason, GenerationOptions, ModelCapabilities, ModelInfo,
    ProviderDescriptor, ProviderId, ProviderKind, TokenUsage,
};
