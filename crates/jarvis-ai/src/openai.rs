//! OpenAI-compatible API provider (LM Studio, llama.cpp server, vLLM,
//! LocalAI, ...).
//!
//! Endpoints relative to the base URL (which usually ends in `/v1`):
//! `GET models`, `POST chat/completions` (SSE streaming), `POST embeddings`.
//! The API does not report model capabilities, so they are inferred
//! conservatively from the model id, unless the creator of the provider knows
//! them ([`OpenAiCompatibleProvider::with_capabilities`]).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::http::{self, ChunkDecoder, HttpClient, SseParser};
use crate::{
    AiError, AiProvider, ChatChunk, ChatMessage, ChatRequest, ChatStream, EmbeddingRequest,
    EmbeddingResponse, FinishReason, ModelCapabilities, ModelInfo, ProviderDescriptor, TokenUsage,
};

#[derive(Debug)]
pub struct OpenAiCompatibleProvider {
    descriptor: ProviderDescriptor,
    base: Url,
    http: Arc<HttpClient>,
    /// Bearer token sent with every request (e.g. the per-launch key of the
    /// embedded llama.cpp server). Never logged.
    api_key: Option<String>,
    /// Capabilities reported for every model instead of the guess from the
    /// model id (servers that are known to serve one kind of model).
    capabilities: Option<ModelCapabilities>,
}

impl OpenAiCompatibleProvider {
    pub fn new(descriptor: ProviderDescriptor, base: Url, http: Arc<HttpClient>) -> Self {
        Self {
            descriptor,
            base: http::normalize_base(base),
            http,
            api_key: None,
            capabilities: None,
        }
    }

    /// Sends `Authorization: Bearer <key>` with every request.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Reports `capabilities` for every listed model.
    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    fn url(&self, path: &str) -> Result<Url, AiError> {
        http::endpoint(&self.base, path)
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }

    async fn fetch_models(&self) -> Result<ModelsResponse, AiError> {
        let client = self.http.get()?;
        let url = self.url("models")?;
        http::send_json(|| self.authorize(client.get(url.clone()))).await
    }
}

#[async_trait]
impl AiProvider for OpenAiCompatibleProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        self.fetch_models().await.map(|_| ())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(self
            .fetch_models()
            .await?
            .data
            .into_iter()
            .map(|model| ModelInfo {
                capabilities: self
                    .capabilities
                    .unwrap_or_else(|| infer_capabilities(&model.id)),
                display_name: model.id.clone(),
                id: model.id,
                provider_id: self.descriptor.id.clone(),
                context_window: None,
            })
            .collect())
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        let client = self.http.get()?;
        let url = self.url("chat/completions")?;

        let messages: Vec<Value> = request.messages.iter().map(message_json).collect();
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": true,
        });
        if let Some(temperature) = request.options.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(max_tokens) = request.options.max_output_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        let response = self
            .authorize(client.post(url))
            .json(&body)
            .send()
            .await
            .map_err(|err| http::map_transport_error(&err))?;
        if !response.status().is_success() {
            return Err(http::status_error(response, Some(&request.model)).await);
        }
        Ok(http::decode_stream(response, SseChatDecoder::default()))
    }

    async fn embeddings(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse, AiError> {
        let client = self.http.get()?;
        let url = self.url("embeddings")?;
        let body = json!({ "model": request.model, "input": request.input });
        let mut response: EmbeddingsResponse =
            http::send_json(|| self.authorize(client.post(url.clone())).json(&body)).await?;
        if response.data.len() != request.input.len() {
            return Err(AiError::Protocol(format!(
                "expected {} embeddings, got {}",
                request.input.len(),
                response.data.len()
            )));
        }
        response.data.sort_by_key(|item| item.index);
        Ok(EmbeddingResponse {
            vectors: response
                .data
                .into_iter()
                .map(|item| item.embedding)
                .collect(),
        })
    }
}

/// The OpenAI API has no capability discovery: models whose id names an
/// embedding model are treated as embedding-only, everything else as a
/// streaming chat model. Vision and tool calling are never assumed.
fn infer_capabilities(model_id: &str) -> ModelCapabilities {
    let id = model_id.to_ascii_lowercase();
    if id.contains("embed") || id.contains("bge-") || id.contains("e5-") {
        ModelCapabilities {
            embeddings: true,
            ..ModelCapabilities::default()
        }
    } else {
        ModelCapabilities {
            chat: true,
            streaming: true,
            ..ModelCapabilities::default()
        }
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct CompletionChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Option<ChunkDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

/// Decodes `chat/completions` server-sent events.
#[derive(Debug, Default)]
struct SseChatDecoder {
    sse: SseParser,
    finish_reason: Option<FinishReason>,
    usage: Option<TokenUsage>,
}

impl SseChatDecoder {
    fn done(&self) -> ChatChunk {
        ChatChunk::Done {
            finish_reason: self.finish_reason.unwrap_or(FinishReason::Stop),
            usage: self.usage,
        }
    }

    fn decode_event(&mut self, data: &str) -> Result<Option<ChatChunk>, AiError> {
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(Some(self.done()));
        }
        if data.is_empty() {
            return Ok(None);
        }
        let chunk: CompletionChunk = serde_json::from_str(data)
            .map_err(|err| AiError::Protocol(format!("invalid chat stream event: {err}")))?;
        if let Some(error) = chunk.error {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .map_or_else(|| error.to_string(), str::to_owned);
            return Err(AiError::Unavailable(message));
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(TokenUsage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
            });
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(None);
        };
        if let Some(reason) = choice.finish_reason {
            self.finish_reason = Some(if reason == "length" {
                FinishReason::Length
            } else {
                FinishReason::Stop
            });
        }
        Ok(choice
            .delta
            .and_then(|delta| delta.content)
            .filter(|content| !content.is_empty())
            .map(ChatChunk::Delta))
    }
}

impl ChunkDecoder for SseChatDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ChatChunk>, AiError> {
        let mut chunks = Vec::new();
        for event in self.sse.push(bytes)? {
            chunks.extend(self.decode_event(&event)?);
        }
        Ok(chunks)
    }

    fn finish(&mut self) -> Result<Vec<ChatChunk>, AiError> {
        let mut chunks = Vec::new();
        for event in self.sse.finish()? {
            chunks.extend(self.decode_event(&event)?);
        }
        // Some servers close the stream without `[DONE]`.
        if !chunks.iter().any(|c| matches!(c, ChatChunk::Done { .. })) {
            chunks.push(self.done());
        }
        Ok(chunks)
    }
}

/// A message in the Chat Completions format; images become `image_url`
/// content parts.
fn message_json(message: &ChatMessage) -> Value {
    if message.images.is_empty() {
        return json!({ "role": message.role, "content": message.content });
    }
    let mut parts = vec![json!({ "type": "text", "text": message.content })];
    parts.extend(
        message
            .images
            .iter()
            .map(|image| json!({ "type": "image_url", "image_url": { "url": image.data_url() } })),
    );
    json!({ "role": message.role, "content": parts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_sse_deltas_finish_reason_and_usage() {
        let mut decoder = SseChatDecoder::default();
        let chunks = decoder
            .feed(
                b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n\
data: [DONE]\n\n",
            )
            .expect("decode");
        assert_eq!(
            chunks,
            vec![
                ChatChunk::Delta("Hi".into()),
                ChatChunk::Done {
                    finish_reason: FinishReason::Length,
                    usage: Some(TokenUsage {
                        prompt_tokens: Some(3),
                        completion_tokens: Some(1)
                    }),
                },
            ]
        );
    }

    #[test]
    fn finishes_streams_without_done_marker() {
        let mut decoder = SseChatDecoder::default();
        decoder
            .feed(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}",
            )
            .expect("feed");
        let chunks = decoder.finish().expect("finish");
        assert_eq!(chunks[0], ChatChunk::Delta("x".into()));
        assert!(matches!(
            chunks[1],
            ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                ..
            }
        ));
    }

    #[test]
    fn surfaces_error_events() {
        let mut decoder = SseChatDecoder::default();
        let err = decoder
            .feed(b"data: {\"error\":{\"message\":\"context length exceeded\"}}\n\n")
            .expect_err("error event");
        assert_eq!(err, AiError::Unavailable("context length exceeded".into()));
    }

    #[test]
    fn infers_capabilities_conservatively() {
        let chat = infer_capabilities("qwen2.5-7b-instruct");
        assert!(chat.chat && chat.streaming && !chat.vision && !chat.tool_calling);
        let embed = infer_capabilities("text-embedding-nomic-embed-text-v1.5");
        assert!(embed.embeddings && !embed.chat);
    }
}
