use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Stable identifier of a registered provider, e.g. `ollama` or `diagnostic-echo`.
///
/// Serde serializes newtype structs as their inner value, so this is a plain
/// string on the wire.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export_to = "ai.ts")]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// How a provider is hosted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub enum ProviderKind {
    /// Built-in test/diagnostic provider. Never backed by a real model.
    Diagnostic,
    /// External local server (Ollama, LM Studio, llama.cpp server, ...).
    LocalServer,
    /// Runtime managed by JARVIS itself (embedded llama.cpp).
    Embedded,
    /// Third-party cloud API.
    Cloud,
}

/// Where request data ends up. Used by privacy checks and model routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub enum DataLocality {
    /// Data never leaves this machine.
    OnDevice,
    /// Data is sent to a server on the local network.
    LocalNetwork,
    /// Data is sent to a third-party service on the internet.
    ThirdPartyCloud,
}

/// Static description of a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub struct ProviderDescriptor {
    pub id: ProviderId,
    pub display_name: String,
    pub kind: ProviderKind,
    pub locality: DataLocality,
}

/// What a model can do. Never assume a capability that is not listed here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub struct ModelCapabilities {
    pub chat: bool,
    pub streaming: bool,
    pub embeddings: bool,
    pub tool_calling: bool,
    pub vision: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub struct ModelInfo {
    /// Provider-specific model identifier.
    pub id: String,
    pub display_name: String,
    pub provider_id: ProviderId,
    /// Context window in tokens, when the provider reports it.
    pub context_window: Option<u32>,
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ChatRole::User, content)
    }
}

/// Sampling parameters. `None` means "provider default".
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationOptions {
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
}

/// A fully resolved chat request addressed to one provider and model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub options: GenerationOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub enum FinishReason {
    /// The model finished its answer.
    Stop,
    /// The output token limit was reached.
    Length,
    /// The request was cancelled by the user or the application.
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "ai.ts")]
pub struct TokenUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

/// One item of a streamed chat response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatChunk {
    /// Incremental text.
    Delta(String),
    /// Terminal item. Providers should always send it; consumers treat a
    /// stream that ends without it as finished with [`FinishReason::Stop`].
    Done {
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
    },
}

/// A complete (non-streamed) chat response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub content: String,
    pub finish_reason: FinishReason,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingResponse {
    /// One vector per input, in input order.
    pub vectors: Vec<Vec<f32>>,
}
