//! Anthropic Messages API provider.
//!
//! Talks to the official Anthropic API (`https://api.anthropic.com/v1`).
//! Endpoints: `GET models` (errors are propagated),
//! `POST messages` (SSE streaming).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::http::{self, ChunkDecoder, HttpClient, SseParser};
use crate::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatRole, ChatStream, EmbeddingRequest,
    EmbeddingResponse, FinishReason, ModelCapabilities, ModelInfo, ProviderDescriptor, TokenUsage,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicProvider {
    descriptor: ProviderDescriptor,
    base: Url,
    http: Arc<HttpClient>,
    api_key: Option<String>,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("descriptor", &self.descriptor)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    pub fn new(descriptor: ProviderDescriptor, base: Url, http: Arc<HttpClient>) -> Self {
        Self {
            descriptor,
            base: http::normalize_base(base),
            http,
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    fn url(&self, path: &str) -> Result<Url, AiError> {
        http::endpoint(&self.base, path)
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let req = request.header("anthropic-version", ANTHROPIC_VERSION);
        match &self.api_key {
            Some(key) => req.header("x-api-key", key),
            None => req,
        }
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let client = self.http.get()?;
        let url = self.url("models")?;
        let res: Result<AnthropicModelsResponse, AiError> =
            http::send_json(|| self.authorize(client.get(url.clone()))).await;

        match res {
            Ok(body) => Ok(body
                .data
                .into_iter()
                .map(|m| ModelInfo {
                    capabilities: infer_anthropic_capabilities(&m.id),
                    display_name: m.display_name.unwrap_or_else(|| m.id.clone()),
                    id: m.id,
                    provider_id: self.descriptor.id.clone(),
                    context_window: Some(200_000),
                })
                .collect()),
            Err(err) => Err(err),
        }
    }
}

fn infer_anthropic_capabilities(_model_id: &str) -> ModelCapabilities {
    ModelCapabilities {
        chat: true,
        streaming: true,
        vision: true,
        tool_calling: true,
        embeddings: false,
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        let _ = self.fetch_models().await?;
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        self.fetch_models().await
    }

    async fn capabilities(&self, model: &str) -> Result<ModelCapabilities, AiError> {
        Ok(infer_anthropic_capabilities(model))
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<ChatStream, AiError> {
        let client = self.http.get()?;
        let url = self.url("messages")?;

        let mut system_parts = Vec::new();
        let mut messages = Vec::new();

        for msg in req.messages {
            match msg.role {
                ChatRole::System => system_parts.push(msg.content),
                ChatRole::User => messages.push(json!({
                    "role": "user",
                    "content": msg.content,
                })),
                ChatRole::Assistant => messages.push(json!({
                    "role": "assistant",
                    "content": msg.content,
                })),
            }
        }

        if messages.is_empty() {
            return Err(AiError::InvalidRequest(
                "Anthropic requires at least one user or assistant message".into(),
            ));
        }

        let max_tokens = req.options.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": true,
        });

        if !system_parts.is_empty() {
            body["system"] = json!(system_parts.join("\n\n"));
        }
        if let Some(temp) = req.options.temperature {
            body["temperature"] = json!(temp);
        }

        let response = self
            .authorize(client.post(url))
            .json(&body)
            .send()
            .await
            .map_err(|err| http::map_transport_error(&err))?;

        if !response.status().is_success() {
            return Err(http::status_error(response, Some(&req.model)).await);
        }

        Ok(http::decode_stream(
            response,
            AnthropicSseDecoder::default(),
        ))
    }

    async fn embeddings(&self, _req: EmbeddingRequest) -> Result<EmbeddingResponse, AiError> {
        Err(AiError::Unsupported {
            provider: self.descriptor.id.clone(),
            operation: "embeddings",
        })
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicModelsResponse {
    #[serde(default)]
    data: Vec<AnthropicModelEntry>,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelEntry {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Default)]
struct AnthropicSseDecoder {
    sse: SseParser,
    finish_reason: Option<FinishReason>,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

impl AnthropicSseDecoder {
    fn decode_event(&mut self, data: &str) -> Result<Option<ChatChunk>, AiError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(data)
            .map_err(|err| AiError::Protocol(format!("invalid Anthropic SSE JSON: {err}")))?;

        let event_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "error" => {
                let msg = value
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Anthropic API error");
                Err(AiError::Unavailable(msg.to_owned()))
            }
            "message_start" => {
                if let Some(usage) = value.get("message").and_then(|m| m.get("usage"))
                    && let Some(input) = usage.get("input_tokens").and_then(|i| i.as_u64())
                {
                    self.prompt_tokens = Some(input as u32);
                }
                Ok(None)
            }
            "content_block_delta" => {
                let delta_text = value
                    .get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str());
                Ok(delta_text
                    .filter(|t| !t.is_empty())
                    .map(|t| ChatChunk::Delta(t.to_owned())))
            }
            "message_delta" => {
                if let Some(stop_reason) = value
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|s| s.as_str())
                {
                    self.finish_reason = Some(if stop_reason == "max_tokens" {
                        FinishReason::Length
                    } else {
                        FinishReason::Stop
                    });
                }
                if let Some(usage) = value.get("usage")
                    && let Some(output) = usage.get("output_tokens").and_then(|o| o.as_u64())
                {
                    self.completion_tokens = Some(output as u32);
                }
                Ok(None)
            }
            "message_stop" => Ok(Some(ChatChunk::Done {
                finish_reason: self.finish_reason.unwrap_or(FinishReason::Stop),
                usage: Some(TokenUsage {
                    prompt_tokens: self.prompt_tokens,
                    completion_tokens: self.completion_tokens,
                }),
            })),
            _ => Ok(None),
        }
    }
}

impl ChunkDecoder for AnthropicSseDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ChatChunk>, AiError> {
        let mut chunks = Vec::new();
        for event in self.sse.push(bytes)? {
            if let Some(chunk) = self.decode_event(&event)? {
                chunks.push(chunk);
            }
        }
        Ok(chunks)
    }

    fn finish(&mut self) -> Result<Vec<ChatChunk>, AiError> {
        let mut chunks = Vec::new();
        for event in self.sse.finish()? {
            if let Some(chunk) = self.decode_event(&event)? {
                chunks.push(chunk);
            }
        }
        if !chunks.iter().any(|c| matches!(c, ChatChunk::Done { .. })) {
            chunks.push(ChatChunk::Done {
                finish_reason: self.finish_reason.unwrap_or(FinishReason::Stop),
                usage: Some(TokenUsage {
                    prompt_tokens: self.prompt_tokens,
                    completion_tokens: self.completion_tokens,
                }),
            });
        }
        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_anthropic_sse_events() {
        let mut decoder = AnthropicSseDecoder::default();
        let events = b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25}}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":10}}\n\n\
data: {\"type\":\"message_stop\"}\n\n";

        let chunks = decoder.feed(events).expect("decode");
        assert_eq!(
            chunks,
            vec![
                ChatChunk::Delta("Hello".into()),
                ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                    usage: Some(TokenUsage {
                        prompt_tokens: Some(25),
                        completion_tokens: Some(10)
                    }),
                }
            ]
        );
    }
}
