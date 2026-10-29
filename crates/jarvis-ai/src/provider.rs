use std::fmt;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::{
    AiError, ChatChunk, ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse,
    FinishReason, ModelCapabilities, ModelInfo, ProviderDescriptor,
};

/// A stream of chat chunks.
///
/// **Cancellation contract:** dropping the stream must stop all work for the
/// request (close the HTTP connection, stop generation, ...). This is how
/// JARVIS cancels requests, together with its own cancellation tokens.
pub type ChatStream = BoxStream<'static, Result<ChatChunk, AiError>>;

/// Interface implemented by every AI backend.
///
/// Not every backend supports every operation: optional operations have
/// default implementations that return [`AiError::Unsupported`], and callers
/// must check [`ModelCapabilities`] instead of assuming support.
///
/// Implementations must not retry non-idempotent operations on their own.
#[async_trait]
pub trait AiProvider: Send + Sync + fmt::Debug {
    fn descriptor(&self) -> &ProviderDescriptor;

    /// Whether this adapter implements the native tools transport. Model
    /// support must also be checked independently.
    fn supports_native_tools(&self) -> bool {
        false
    }

    /// Cheap readiness probe (e.g. `GET /api/version`). Must not load models.
    async fn health_check(&self) -> Result<(), AiError>;

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError>;

    /// Capabilities of a specific model.
    async fn capabilities(&self, model: &str) -> Result<ModelCapabilities, AiError> {
        self.list_models()
            .await?
            .into_iter()
            .find(|info| info.id == model)
            .map(|info| info.capabilities)
            .ok_or_else(|| AiError::ModelNotFound {
                model: model.to_owned(),
            })
    }

    /// Starts a streamed chat completion.
    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError>;

    /// A native tool turn. Callers must check model capabilities and apply
    /// their security policy to every returned call. No tools are executed
    /// here. Dropping the future cancels the request; callers own deadlines.
    async fn chat_with_tools(
        &self,
        _request: crate::ToolChatRequest,
    ) -> Result<crate::ToolChatResponse, AiError> {
        Err(AiError::Unsupported {
            provider: self.descriptor().id.clone(),
            operation: "tool calling",
        })
    }

    /// Non-streamed chat completion. The default implementation collects
    /// [`AiProvider::stream_chat`].
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, AiError> {
        let mut stream = self.stream_chat(request).await?;
        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk? {
                ChatChunk::Delta(text) => content.push_str(&text),
                ChatChunk::Done {
                    finish_reason,
                    usage,
                } => {
                    return Ok(ChatResponse {
                        content,
                        finish_reason,
                        usage,
                    });
                }
            }
        }
        Ok(ChatResponse {
            content,
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }

    async fn embeddings(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse, AiError> {
        let _ = request;
        Err(AiError::Unsupported {
            provider: self.descriptor().id.clone(),
            operation: "embeddings",
        })
    }
}
