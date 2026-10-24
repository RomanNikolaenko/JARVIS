//! Google Gemini API provider.
//!
//! Talks to the Google AI Studio / Gemini REST API (`https://generativelanguage.googleapis.com/v1beta`).
//! Endpoints: `GET models` (errors are propagated),
//! `POST models/{model}:streamGenerateContent?alt=sse` (SSE streaming).

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

pub struct GeminiProvider {
    descriptor: ProviderDescriptor,
    base: Url,
    http: Arc<HttpClient>,
    api_key: Option<String>,
}

impl std::fmt::Debug for GeminiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiProvider")
            .field("descriptor", &self.descriptor)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl GeminiProvider {
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
        match &self.api_key {
            Some(key) => request.header("x-goog-api-key", key),
            None => request,
        }
    }

    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let client = self.http.get()?;
        let url = self.url("models")?;
        let res: Result<GeminiModelsResponse, AiError> =
            http::send_json(|| self.authorize(client.get(url.clone()))).await;

        match res {
            Ok(body) => Ok(body
                .models
                .into_iter()
                .filter(|m| {
                    m.supported_generation_methods
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .any(|method| method == "generateContent")
                })
                .map(|m| {
                    let clean_id = m.name.strip_prefix("models/").unwrap_or(&m.name).to_owned();
                    ModelInfo {
                        capabilities: infer_gemini_capabilities(&clean_id),
                        display_name: m.display_name.unwrap_or_else(|| clean_id.clone()),
                        id: clean_id,
                        provider_id: self.descriptor.id.clone(),
                        context_window: m.input_token_limit.or(Some(1_000_000)),
                    }
                })
                .collect()),
            Err(err) => Err(err),
        }
    }
}

fn infer_gemini_capabilities(model_id: &str) -> ModelCapabilities {
    let lower = model_id.to_ascii_lowercase();
    if lower.contains("embedding") {
        ModelCapabilities {
            embeddings: true,
            ..ModelCapabilities::default()
        }
    } else {
        ModelCapabilities {
            chat: true,
            streaming: true,
            vision: true,
            tool_calling: true,
            embeddings: false,
        }
    }
}

#[async_trait]
impl AiProvider for GeminiProvider {
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
        Ok(infer_gemini_capabilities(model))
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<ChatStream, AiError> {
        let client = self.http.get()?;
        let model_path = format!("models/{}:streamGenerateContent?alt=sse", req.model);
        let url = self.url(&model_path)?;

        let mut contents = Vec::new();
        let mut system_text = String::new();

        for msg in req.messages {
            match msg.role {
                ChatRole::System => {
                    if !system_text.is_empty() {
                        system_text.push_str("\n\n");
                    }
                    system_text.push_str(&msg.content);
                }
                ChatRole::User => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{"text": msg.content}]
                    }));
                }
                ChatRole::Assistant => {
                    contents.push(json!({
                        "role": "model",
                        "parts": [{"text": msg.content}]
                    }));
                }
            }
        }

        if contents.is_empty() {
            return Err(AiError::InvalidRequest(
                "Gemini requires at least one user or assistant message".into(),
            ));
        }

        let mut body = json!({
            "contents": contents,
        });

        if !system_text.is_empty() {
            body["systemInstruction"] = json!({
                "parts": [{"text": system_text}]
            });
        }

        let mut gen_config = json!({});
        if let Some(temp) = req.options.temperature {
            gen_config["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = req.options.max_output_tokens {
            gen_config["maxOutputTokens"] = json!(max_tokens);
        }
        if gen_config.as_object().is_some_and(|o| !o.is_empty()) {
            body["generationConfig"] = gen_config;
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

        Ok(http::decode_stream(response, GeminiSseDecoder::default()))
    }

    async fn embeddings(&self, _req: EmbeddingRequest) -> Result<EmbeddingResponse, AiError> {
        Err(AiError::Unsupported {
            provider: self.descriptor.id.clone(),
            operation: "embeddings",
        })
    }
}

#[derive(Debug, Deserialize)]
struct GeminiModelsResponse {
    #[serde(default)]
    models: Vec<GeminiModelEntry>,
}

#[derive(Debug, Deserialize)]
struct GeminiModelEntry {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    input_token_limit: Option<u32>,
    #[serde(default)]
    supported_generation_methods: Option<Vec<String>>,
}

#[derive(Debug, Default)]
struct GeminiSseDecoder {
    sse: SseParser,
    finish_reason: Option<FinishReason>,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

impl GeminiSseDecoder {
    fn decode_event(&mut self, data: &str) -> Result<Option<ChatChunk>, AiError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = serde_json::from_str(data)
            .map_err(|err| AiError::Protocol(format!("invalid Gemini SSE JSON: {err}")))?;

        if let Some(err) = value.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Gemini API error");
            return Err(AiError::Unavailable(msg.to_owned()));
        }

        if let Some(usage) = value.get("usageMetadata") {
            if let Some(p) = usage.get("promptTokenCount").and_then(|c| c.as_u64()) {
                self.prompt_tokens = Some(p as u32);
            }
            if let Some(c) = usage.get("candidatesTokenCount").and_then(|c| c.as_u64()) {
                self.completion_tokens = Some(c as u32);
            }
        }

        let mut delta_text = String::new();
        if let Some(candidates) = value.get("candidates").and_then(|c| c.as_array())
            && let Some(first) = candidates.first()
        {
            if let Some(parts) = first
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        delta_text.push_str(text);
                    }
                }
            }
            if let Some(reason) = first.get("finishReason").and_then(|r| r.as_str()) {
                self.finish_reason = Some(match reason {
                    "MAX_TOKENS" => FinishReason::Length,
                    _ => FinishReason::Stop,
                });
            }
        }

        if !delta_text.is_empty() {
            Ok(Some(ChatChunk::Delta(delta_text)))
        } else {
            Ok(None)
        }
    }
}

impl ChunkDecoder for GeminiSseDecoder {
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
    fn decodes_gemini_sse_events() {
        let mut decoder = GeminiSseDecoder::default();
        let events = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello from Gemini\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":12,\"candidatesTokenCount\":8}}\n\n";

        let chunks = decoder.feed(events).expect("decode");
        assert_eq!(chunks, vec![ChatChunk::Delta("Hello from Gemini".into())]);

        let finish_chunks = decoder.finish().expect("finish");
        assert_eq!(
            finish_chunks,
            vec![ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    prompt_tokens: Some(12),
                    completion_tokens: Some(8)
                }),
            }]
        );
    }
}
