use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream;

use crate::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatRole, ChatStream, DataLocality, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId, ProviderKind, TokenUsage,
};

/// Diagnostic provider that streams the last user message back, word by word.
///
/// It exists to verify the streaming, cancellation and error-handling paths
/// without any model installed. It is labelled as [`ProviderKind::Diagnostic`]
/// everywhere in the UI and never pretends to be an AI model.
#[derive(Debug)]
pub struct EchoProvider {
    descriptor: ProviderDescriptor,
    chunk_delay: Duration,
}

impl Default for EchoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoProvider {
    pub const ID: &'static str = "diagnostic-echo";
    pub const MODEL: &'static str = "echo";
    /// Prefix that makes echo responses unmistakable in the chat transcript.
    pub const PREFIX: &'static str = "[echo] ";

    pub fn new() -> Self {
        Self::with_chunk_delay(Duration::from_millis(35))
    }

    /// Creates an echo provider with a custom delay between streamed chunks.
    pub fn with_chunk_delay(chunk_delay: Duration) -> Self {
        Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new(Self::ID),
                display_name: "Diagnostic echo".to_owned(),
                kind: ProviderKind::Diagnostic,
                locality: DataLocality::OnDevice,
            },
            chunk_delay,
        }
    }

    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            id: Self::MODEL.to_owned(),
            display_name: "Echo (diagnostic)".to_owned(),
            provider_id: self.descriptor.id.clone(),
            context_window: None,
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                ..ModelCapabilities::default()
            },
        }
    }
}

#[async_trait]
impl AiProvider for EchoProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![self.model_info()])
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        if request.model != Self::MODEL {
            return Err(AiError::ModelNotFound {
                model: request.model,
            });
        }
        let last_user_message = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::User)
            .ok_or_else(|| AiError::InvalidRequest("no user message to echo".to_owned()))?;

        let reply = format!("{}{}", Self::PREFIX, last_user_message.content);
        let chunks: Vec<String> = reply.split_inclusive(' ').map(str::to_owned).collect();
        let completion_tokens = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
        let delay = self.chunk_delay;

        let deltas = stream::iter(chunks).then(move |chunk| async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Ok(ChatChunk::Delta(chunk))
        });
        let done = stream::once(async move {
            Ok(ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    prompt_tokens: None,
                    completion_tokens: Some(completion_tokens),
                }),
            })
        });

        Ok(deltas.chain(done).boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatMessage, EmbeddingRequest, GenerationOptions};

    fn request(model: &str, messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            model: model.to_owned(),
            messages,
            options: GenerationOptions::default(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn streams_last_user_message_word_by_word() {
        let provider = EchoProvider::new();
        let stream = provider
            .stream_chat(request(
                EchoProvider::MODEL,
                vec![
                    ChatMessage::user("first"),
                    ChatMessage::new(ChatRole::Assistant, "ignored"),
                    ChatMessage::user("hello there JARVIS"),
                ],
            ))
            .await
            .expect("stream");

        let chunks: Vec<ChatChunk> = stream.map(|c| c.expect("chunk")).collect().await;
        let (last, deltas) = chunks.split_last().expect("at least one chunk");

        let text: String = deltas
            .iter()
            .map(|chunk| match chunk {
                ChatChunk::Delta(text) => text.as_str(),
                ChatChunk::Done { .. } => panic!("done before end of stream"),
            })
            .collect();
        assert_eq!(text, "[echo] hello there JARVIS");
        assert_eq!(deltas.len(), 4);
        assert_eq!(
            *last,
            ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    prompt_tokens: None,
                    completion_tokens: Some(4)
                }),
            }
        );
    }

    #[tokio::test]
    async fn default_chat_collects_the_stream() {
        let provider = EchoProvider::with_chunk_delay(Duration::ZERO);
        let response = provider
            .chat(request(
                EchoProvider::MODEL,
                vec![ChatMessage::user("ping")],
            ))
            .await
            .expect("response");
        assert_eq!(response.content, "[echo] ping");
        assert_eq!(response.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn rejects_unknown_models_and_missing_user_messages() {
        let provider = EchoProvider::new();

        let unknown = provider
            .stream_chat(request("gpt-x", vec![ChatMessage::user("hi")]))
            .await;
        assert!(matches!(unknown, Err(AiError::ModelNotFound { .. })));

        let no_user = provider
            .stream_chat(request(
                EchoProvider::MODEL,
                vec![ChatMessage::new(ChatRole::System, "rules")],
            ))
            .await;
        assert!(matches!(no_user, Err(AiError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn reports_capabilities_and_unsupported_operations() {
        let provider = EchoProvider::new();
        let caps = provider
            .capabilities(EchoProvider::MODEL)
            .await
            .expect("caps");
        assert!(caps.chat && caps.streaming);
        assert!(!caps.embeddings && !caps.tool_calling && !caps.vision);

        let missing = provider.capabilities("other").await;
        assert!(matches!(missing, Err(AiError::ModelNotFound { .. })));

        let embeddings = provider
            .embeddings(EmbeddingRequest {
                model: EchoProvider::MODEL.to_owned(),
                input: vec!["text".to_owned()],
            })
            .await;
        assert!(matches!(
            embeddings,
            Err(AiError::Unsupported {
                operation: "embeddings",
                ..
            })
        ));
    }

    #[test]
    fn descriptor_is_clearly_diagnostic_and_local() {
        let provider = EchoProvider::new();
        let descriptor = provider.descriptor();
        assert_eq!(descriptor.kind, ProviderKind::Diagnostic);
        assert_eq!(descriptor.locality, DataLocality::OnDevice);
    }
}
