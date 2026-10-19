//! Ollama native API provider.
//!
//! Endpoints: `GET /api/version` (health), `GET /api/tags` (models),
//! `POST /api/show` (capabilities, context length), `POST /api/chat`
//! (NDJSON streaming), `POST /api/embed` (embeddings).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::http::{self, ChunkDecoder, HttpClient, LineBuffer};
use crate::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatStream, EmbeddingRequest, EmbeddingResponse,
    FinishReason, ModelCapabilities, ModelInfo, ProviderDescriptor, TokenUsage,
};

/// Upper bound of models whose details are fetched in one listing.
const MAX_DETAILED_MODELS: usize = 64;
const SHOW_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct OllamaProvider {
    descriptor: ProviderDescriptor,
    base: Url,
    http: Arc<HttpClient>,
}

impl OllamaProvider {
    pub fn new(descriptor: ProviderDescriptor, base: Url, http: Arc<HttpClient>) -> Self {
        Self {
            descriptor,
            base: http::normalize_base(base),
            http,
        }
    }

    fn url(&self, path: &str) -> Result<Url, AiError> {
        http::endpoint(&self.base, path)
    }

    async fn show(&self, model: &str) -> Result<ShowResponse, AiError> {
        let client = self.http.get()?;
        let url = self.url("api/show")?;
        let response = client
            .post(url)
            .timeout(SHOW_TIMEOUT)
            .json(&json!({ "model": model }))
            .send()
            .await
            .map_err(|err| http::map_transport_error(&err))?;
        if !response.status().is_success() {
            return Err(http::status_error(response, Some(model)).await);
        }
        response
            .json::<ShowResponse>()
            .await
            .map_err(|err| AiError::Protocol(format!("unexpected /api/show response: {err}")))
    }

    fn model_info(&self, tag: &TagModel, show: Option<&ShowResponse>) -> ModelInfo {
        let capabilities = show
            .and_then(ShowResponse::capabilities)
            .unwrap_or_else(|| guess_capabilities(&tag.name));
        let mut display_name = tag.name.clone();
        if let Some(details) = &tag.details {
            let extras: Vec<&str> = [
                details.parameter_size.as_deref(),
                details.quantization_level.as_deref(),
            ]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
            if !extras.is_empty() {
                display_name = format!("{} · {}", tag.name, extras.join(" "));
            }
        }
        ModelInfo {
            id: tag.name.clone(),
            display_name,
            provider_id: self.descriptor.id.clone(),
            context_window: show.and_then(ShowResponse::context_length),
            capabilities,
        }
    }
}

#[async_trait]
impl AiProvider for OllamaProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        let client = self.http.get()?;
        let url = self.url("api/version")?;
        http::send_json::<Value, _>(|| client.get(url.clone()))
            .await
            .map(|_| ())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let client = self.http.get()?;
        let url = self.url("api/tags")?;
        let tags: TagsResponse = http::send_json(|| client.get(url.clone())).await?;

        let details = futures::future::join_all(
            tags.models
                .iter()
                .take(MAX_DETAILED_MODELS)
                .map(|tag| self.show(&tag.name)),
        )
        .await;

        Ok(tags
            .models
            .iter()
            .enumerate()
            .map(|(index, tag)| {
                let show = details.get(index).and_then(|result| result.as_ref().ok());
                self.model_info(tag, show)
            })
            .collect())
    }

    async fn capabilities(&self, model: &str) -> Result<ModelCapabilities, AiError> {
        let show = self.show(model).await?;
        Ok(show
            .capabilities()
            .unwrap_or_else(|| guess_capabilities(model)))
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        let client = self.http.get()?;
        let url = self.url("api/chat")?;

        let mut options = serde_json::Map::new();
        if let Some(temperature) = request.options.temperature {
            options.insert("temperature".into(), json!(temperature));
        }
        if let Some(max_tokens) = request.options.max_output_tokens {
            options.insert("num_predict".into(), json!(max_tokens));
        }
        let body = json!({
            "model": request.model,
            "messages": request.messages,
            "stream": true,
            "options": options,
        });

        let response = client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|err| http::map_transport_error(&err))?;
        if !response.status().is_success() {
            return Err(http::status_error(response, Some(&request.model)).await);
        }
        Ok(http::decode_stream(response, NdjsonChatDecoder::default()))
    }

    async fn embeddings(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse, AiError> {
        let client = self.http.get()?;
        let url = self.url("api/embed")?;
        let body = json!({ "model": request.model, "input": request.input });
        let response: EmbedResponse =
            http::send_json(|| client.post(url.clone()).json(&body)).await?;
        if response.embeddings.len() != request.input.len() {
            return Err(AiError::Protocol(format!(
                "expected {} embeddings, got {}",
                request.input.len(),
                response.embeddings.len()
            )));
        }
        Ok(EmbeddingResponse {
            vectors: response.embeddings,
        })
    }
}

/// Fallback when the server does not report capabilities (Ollama < 0.6).
fn guess_capabilities(model: &str) -> ModelCapabilities {
    if model.to_ascii_lowercase().contains("embed") {
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
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    details: Option<TagDetails>,
}

#[derive(Debug, Deserialize)]
struct TagDetails {
    #[serde(default)]
    parameter_size: Option<String>,
    #[serde(default)]
    quantization_level: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    model_info: Option<serde_json::Map<String, Value>>,
}

impl ShowResponse {
    fn capabilities(&self) -> Option<ModelCapabilities> {
        if self.capabilities.is_empty() {
            return None;
        }
        let has = |name: &str| self.capabilities.iter().any(|c| c == name);
        Some(ModelCapabilities {
            chat: has("completion"),
            streaming: has("completion"),
            embeddings: has("embedding"),
            tool_calling: has("tools"),
            vision: has("vision"),
        })
    }

    fn context_length(&self) -> Option<u32> {
        self.model_info.as_ref()?.iter().find_map(|(key, value)| {
            key.ends_with(".context_length")
                .then(|| value.as_u64())
                .flatten()
                .and_then(|n| u32::try_from(n).ok())
        })
    }
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct ChatLine {
    #[serde(default)]
    message: Option<ChatLineMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatLineMessage {
    #[serde(default)]
    content: String,
}

/// Decodes Ollama's newline-delimited JSON chat stream.
#[derive(Debug, Default)]
struct NdjsonChatDecoder {
    lines: LineBuffer,
}

impl NdjsonChatDecoder {
    fn decode_line(line: &str) -> Result<Option<ChatChunk>, AiError> {
        if line.trim().is_empty() {
            return Ok(None);
        }
        let parsed: ChatLine = serde_json::from_str(line)
            .map_err(|err| AiError::Protocol(format!("invalid chat stream line: {err}")))?;
        if let Some(error) = parsed.error {
            return Err(AiError::Unavailable(error));
        }
        if parsed.done {
            let finish_reason = match parsed.done_reason.as_deref() {
                Some("length") => FinishReason::Length,
                _ => FinishReason::Stop,
            };
            let usage = (parsed.prompt_eval_count.is_some() || parsed.eval_count.is_some())
                .then_some(TokenUsage {
                    prompt_tokens: parsed.prompt_eval_count,
                    completion_tokens: parsed.eval_count,
                });
            return Ok(Some(ChatChunk::Done {
                finish_reason,
                usage,
            }));
        }
        Ok(parsed
            .message
            .map(|m| m.content)
            .filter(|content| !content.is_empty())
            .map(ChatChunk::Delta))
    }
}

impl ChunkDecoder for NdjsonChatDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ChatChunk>, AiError> {
        let mut chunks = Vec::new();
        for line in self.lines.push(bytes)? {
            chunks.extend(Self::decode_line(&line)?);
        }
        Ok(chunks)
    }

    fn finish(&mut self) -> Result<Vec<ChatChunk>, AiError> {
        match self.lines.finish()? {
            Some(line) => Ok(Self::decode_line(&line)?.into_iter().collect()),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_deltas_and_final_line() {
        let mut decoder = NdjsonChatDecoder::default();
        let chunks = decoder
            .feed(
                br#"{"message":{"role":"assistant","content":"Hel"},"done":false}
{"message":{"role":"assistant","content":"lo"},"done":false}
{"message":{"role":"assistant","content":""},"done":true,"done_reason":"length","prompt_eval_count":5,"eval_count":2}
"#,
            )
            .expect("decode");
        assert_eq!(
            chunks,
            vec![
                ChatChunk::Delta("Hel".into()),
                ChatChunk::Delta("lo".into()),
                ChatChunk::Done {
                    finish_reason: FinishReason::Length,
                    usage: Some(TokenUsage {
                        prompt_tokens: Some(5),
                        completion_tokens: Some(2)
                    }),
                },
            ]
        );
    }

    #[test]
    fn reports_stream_errors() {
        let mut decoder = NdjsonChatDecoder::default();
        let err = decoder
            .feed(b"{\"error\":\"model crashed\"}\n")
            .expect_err("error line");
        assert_eq!(err, AiError::Unavailable("model crashed".into()));
        assert!(matches!(
            NdjsonChatDecoder::default().feed(b"not json\n"),
            Err(AiError::Protocol(_))
        ));
    }

    #[test]
    fn maps_reported_capabilities() {
        let show: ShowResponse = serde_json::from_value(json!({
            "capabilities": ["completion", "vision", "tools"],
            "model_info": { "llama.context_length": 131072, "general.architecture": "llama" }
        }))
        .expect("show");
        let caps = show.capabilities().expect("caps");
        assert!(caps.chat && caps.streaming && caps.vision && caps.tool_calling);
        assert!(!caps.embeddings);
        assert_eq!(show.context_length(), Some(131_072));

        assert!(ShowResponse::default().capabilities().is_none());
        assert!(guess_capabilities("nomic-embed-text").embeddings);
        assert!(guess_capabilities("llama3.2").chat);
    }
}
