//! Provider contract tests. Every network provider must pass the same suite
//! against a scripted server that mimics its wire protocol.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatMessage, ChatRequest, EmbeddingRequest, FinishReason,
    GenerationOptions, HttpProviderFactory, ProviderBackend, ProviderConfig, ProviderFactory,
    ProviderId,
};
use serde_json::{Value, json};
use support::{MockServer, Reply, Request, unused_addr};

const WORDS: [&str; 4] = ["Привіт", ", ", "JARVIS", "!"];

#[derive(Debug, Clone, Default)]
struct Scenario {
    /// Respond to chat requests with this status and an error body.
    chat_status: Option<u16>,
    /// Send the first delta, then keep the connection open.
    hang: bool,
    /// Bytes per network chunk (small values split UTF-8 sequences).
    chunk_size: usize,
}

type Handler = Box<dyn Fn(&Request) -> Reply + Send + Sync>;

struct Backend {
    kind: ProviderBackend,
    base_path: &'static str,
    chat_path: &'static str,
    chat_model: &'static str,
    embed_model: &'static str,
    handler: fn(Scenario) -> Handler,
    /// Checks how generation options are encoded in the request body.
    assert_options: fn(&Value),
}

fn model_of(request: &Request) -> String {
    request.json()["model"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn stream_reply(
    content_type: &'static str,
    body: String,
    scenario: &Scenario,
    first: String,
) -> Reply {
    if scenario.hang {
        return Reply::Stream {
            content_type,
            chunks: vec![(Duration::ZERO, first.into_bytes())],
            hang: true,
        };
    }
    Reply::stream(content_type, &body, scenario.chunk_size.max(1))
}

const OLLAMA: Backend = Backend {
    kind: ProviderBackend::Ollama,
    base_path: "",
    chat_path: "/api/chat",
    chat_model: "llama3.2:3b",
    embed_model: "nomic-embed-text:latest",
    handler: ollama_handler,
    assert_options: |body| {
        assert_eq!(body["options"]["temperature"], json!(0.5));
        assert_eq!(body["options"]["num_predict"], json!(64));
    },
};

fn ollama_handler(scenario: Scenario) -> Handler {
    Box::new(move |req| {
        let known = |m: &str| m == OLLAMA.chat_model || m == OLLAMA.embed_model;
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/api/version") => Reply::Json(200, json!({ "version": "0.12.0" })),
            ("GET", "/api/tags") => Reply::Json(
                200,
                json!({ "models": [
                    { "name": OLLAMA.chat_model,
                      "details": { "parameter_size": "3.2B", "quantization_level": "Q4_K_M" } },
                    { "name": OLLAMA.embed_model }
                ]}),
            ),
            ("POST", "/api/show") => match model_of(req).as_str() {
                m if m == OLLAMA.chat_model => Reply::Json(
                    200,
                    json!({ "capabilities": ["completion", "tools"],
                            "model_info": { "llama.context_length": 131072 } }),
                ),
                m if m == OLLAMA.embed_model => {
                    Reply::Json(200, json!({ "capabilities": ["embedding"] }))
                }
                m => Reply::Json(404, json!({ "error": format!("model '{m}' not found") })),
            },
            ("POST", "/api/chat") => {
                let model = model_of(req);
                if !known(&model) {
                    return Reply::Json(
                        404,
                        json!({ "error": format!("model '{model}' not found") }),
                    );
                }
                if let Some(status) = scenario.chat_status {
                    return Reply::Json(status, json!({ "error": "runner crashed" }));
                }
                let line = |content: &str| {
                    json!({ "message": { "role": "assistant", "content": content }, "done": false })
                        .to_string()
                        + "\n"
                };
                let mut body: String = WORDS.iter().map(|w| line(w)).collect();
                body.push_str(
                    &(json!({ "message": { "role": "assistant", "content": "" }, "done": true,
                              "done_reason": "stop", "prompt_eval_count": 7, "eval_count": 4 })
                    .to_string()
                        + "\n"),
                );
                stream_reply("application/x-ndjson", body, &scenario, line(WORDS[0]))
            }
            ("POST", "/api/embed") => {
                let count = req.json()["input"].as_array().map_or(0, Vec::len);
                Reply::Json(
                    200,
                    json!({ "embeddings": vec![vec![0.1, 0.2, 0.3]; count] }),
                )
            }
            _ => Reply::Json(404, json!({ "error": "not found" })),
        }
    })
}

const OPENAI: Backend = Backend {
    kind: ProviderBackend::OpenAiCompatible,
    base_path: "/v1",
    chat_path: "/v1/chat/completions",
    chat_model: "qwen2.5-7b-instruct",
    embed_model: "text-embedding-nomic-embed-text-v1.5",
    handler: openai_handler,
    assert_options: |body| {
        assert_eq!(body["temperature"], json!(0.5));
        assert_eq!(body["max_tokens"], json!(64));
    },
};

fn openai_handler(scenario: Scenario) -> Handler {
    Box::new(move |req| {
        let known = |m: &str| m == OPENAI.chat_model || m == OPENAI.embed_model;
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/v1/models") => Reply::Json(
                200,
                json!({ "object": "list", "data": [
                    { "id": OPENAI.chat_model, "object": "model" },
                    { "id": OPENAI.embed_model, "object": "model" }
                ]}),
            ),
            ("POST", "/v1/chat/completions") => {
                let model = model_of(req);
                if !known(&model) {
                    return Reply::Json(
                        404,
                        json!({ "error": { "message": format!("model {model} not loaded") } }),
                    );
                }
                if let Some(status) = scenario.chat_status {
                    return Reply::Json(
                        status,
                        json!({ "error": { "message": "runner crashed" } }),
                    );
                }
                let event = |content: &str| {
                    format!(
                        "data: {}\n\n",
                        json!({ "choices": [{ "index": 0, "delta": { "content": content } }] })
                    )
                };
                let mut body: String = WORDS.iter().map(|w| event(w)).collect();
                body.push_str(&format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                            "usage": { "prompt_tokens": 7, "completion_tokens": 4 } })
                ));
                stream_reply("text/event-stream", body, &scenario, event(WORDS[0]))
            }
            ("POST", "/v1/embeddings") => {
                let count = req.json()["input"].as_array().map_or(0, Vec::len);
                let data: Vec<Value> = (0..count)
                    .rev()
                    .map(|index| json!({ "index": index, "embedding": [0.1, 0.2, 0.3] }))
                    .collect();
                Reply::Json(200, json!({ "data": data }))
            }
            _ => Reply::Json(404, json!({ "error": { "message": "not found" } })),
        }
    })
}

fn provider_at(backend: &Backend, base_url: String) -> Arc<dyn AiProvider> {
    HttpProviderFactory::new()
        .create(&ProviderConfig {
            id: ProviderId::new("test"),
            backend: backend.kind,
            display_name: "Test".into(),
            base_url,
            enabled: true,
        })
        .expect("provider")
}

async fn start(backend: &Backend, scenario: Scenario) -> (MockServer, Arc<dyn AiProvider>) {
    let server = MockServer::start((backend.handler)(scenario)).await;
    let provider = provider_at(backend, server.url(backend.base_path));
    (server, provider)
}

fn chat_request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![ChatMessage::user("Привіт")],
        options: GenerationOptions::default(),
    }
}

async fn check_health(backend: &Backend) {
    let (_server, provider) = start(backend, Scenario::default()).await;
    provider.health_check().await.expect("healthy");

    let offline = provider_at(
        backend,
        format!("http://{}{}", unused_addr().await, backend.base_path),
    );
    let err = offline.health_check().await.expect_err("offline");
    assert!(matches!(err, AiError::Unavailable(_)), "{err:?}");
    assert!(err.is_transient());
}

async fn check_list_models(backend: &Backend) {
    let (_server, provider) = start(backend, Scenario::default()).await;
    let models = provider.list_models().await.expect("models");
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|m| m.provider_id.as_str() == "test"));

    let chat = models
        .iter()
        .find(|m| m.id == backend.chat_model)
        .expect("chat model");
    assert!(chat.capabilities.chat && chat.capabilities.streaming);
    let embed = models
        .iter()
        .find(|m| m.id == backend.embed_model)
        .expect("embedding model");
    assert!(embed.capabilities.embeddings && !embed.capabilities.chat);

    let caps = provider
        .capabilities(backend.chat_model)
        .await
        .expect("caps");
    assert!(caps.chat);
}

async fn check_streaming(backend: &Backend) {
    let (server, provider) = start(
        backend,
        Scenario {
            chunk_size: 3,
            ..Scenario::default()
        },
    )
    .await;
    let mut request = chat_request(backend.chat_model);
    request.options = GenerationOptions {
        temperature: Some(0.5),
        max_output_tokens: Some(64),
    };
    let chunks: Vec<ChatChunk> = provider
        .stream_chat(request)
        .await
        .expect("stream")
        .map(|c| c.expect("chunk"))
        .collect()
        .await;

    let (last, deltas) = chunks.split_last().expect("chunks");
    let text: String = deltas
        .iter()
        .map(|c| match c {
            ChatChunk::Delta(t) => t.as_str(),
            ChatChunk::Done { .. } => panic!("Done before the end"),
        })
        .collect();
    assert_eq!(text, WORDS.concat());
    match last {
        ChatChunk::Done {
            finish_reason,
            usage,
        } => {
            assert_eq!(*finish_reason, FinishReason::Stop);
            assert_eq!(usage.and_then(|u| u.completion_tokens), Some(4));
        }
        other => panic!("expected Done, got {other:?}"),
    }

    let sent = server
        .last_request_to(backend.chat_path)
        .expect("chat request")
        .json();
    assert_eq!(sent["model"], backend.chat_model);
    assert_eq!(sent["stream"], true);
    assert_eq!(
        sent["messages"][0],
        json!({ "role": "user", "content": "Привіт" })
    );
    (backend.assert_options)(&sent);
}

async fn check_chat_collects(backend: &Backend) {
    let (_server, provider) = start(backend, Scenario::default()).await;
    let response = provider
        .chat(chat_request(backend.chat_model))
        .await
        .expect("response");
    assert_eq!(response.content, WORDS.concat());
    assert_eq!(response.finish_reason, FinishReason::Stop);
}

async fn check_unknown_model(backend: &Backend) {
    let (_server, provider) = start(backend, Scenario::default()).await;
    let err = provider
        .stream_chat(chat_request("missing-model"))
        .await
        .err()
        .expect("unknown model must fail");
    assert_eq!(
        err,
        AiError::ModelNotFound {
            model: "missing-model".into()
        }
    );
}

async fn check_server_error(backend: &Backend) {
    let (_server, provider) = start(
        backend,
        Scenario {
            chat_status: Some(500),
            ..Scenario::default()
        },
    )
    .await;
    let err = provider
        .stream_chat(chat_request(backend.chat_model))
        .await
        .err()
        .expect("server error");
    match err {
        AiError::Unavailable(message) => assert!(message.contains("runner crashed"), "{message}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

async fn check_cancellation(backend: &Backend) {
    let (server, provider) = start(
        backend,
        Scenario {
            hang: true,
            ..Scenario::default()
        },
    )
    .await;
    let mut stream = provider
        .stream_chat(chat_request(backend.chat_model))
        .await
        .expect("stream");
    let first = stream.next().await.expect("first chunk").expect("ok");
    assert_eq!(first, ChatChunk::Delta(WORDS[0].into()));

    drop(stream);
    assert!(
        server.wait_for_disconnect().await,
        "dropping the stream must close the connection"
    );
}

async fn check_embeddings(backend: &Backend) {
    let (_server, provider) = start(backend, Scenario::default()).await;
    let response = provider
        .embeddings(EmbeddingRequest {
            model: backend.embed_model.to_owned(),
            input: vec!["a".into(), "b".into()],
        })
        .await
        .expect("embeddings");
    assert_eq!(response.vectors.len(), 2);
    assert_eq!(response.vectors[0], vec![0.1, 0.2, 0.3]);
}

macro_rules! contract_suite {
    ($name:ident, $backend:expr) => {
        mod $name {
            use super::*;

            #[tokio::test]
            async fn health_check_reports_availability() {
                check_health(&$backend).await;
            }

            #[tokio::test]
            async fn lists_models_with_capabilities() {
                check_list_models(&$backend).await;
            }

            #[tokio::test]
            async fn streams_chat_across_split_chunks() {
                check_streaming(&$backend).await;
            }

            #[tokio::test]
            async fn chat_collects_the_stream() {
                check_chat_collects(&$backend).await;
            }

            #[tokio::test]
            async fn unknown_models_are_reported() {
                check_unknown_model(&$backend).await;
            }

            #[tokio::test]
            async fn server_errors_are_reported() {
                check_server_error(&$backend).await;
            }

            #[tokio::test]
            async fn dropping_the_stream_closes_the_connection() {
                check_cancellation(&$backend).await;
            }

            #[tokio::test]
            async fn computes_embeddings() {
                check_embeddings(&$backend).await;
            }
        }
    };
}

contract_suite!(ollama, OLLAMA);
contract_suite!(openai_compatible, OPENAI);

#[tokio::test]
async fn openai_compatible_sends_the_api_key_when_configured() {
    use jarvis_ai::{
        DataLocality, HttpClient, OpenAiCompatibleProvider, ProviderDescriptor, ProviderKind,
    };

    let server = MockServer::start(openai_handler(Scenario::default())).await;
    let base = url::Url::parse(&server.url("/v1")).expect("url");
    let descriptor = ProviderDescriptor {
        id: ProviderId::new("runtime"),
        display_name: "Runtime".into(),
        kind: ProviderKind::Embedded,
        locality: DataLocality::OnDevice,
    };
    let provider = OpenAiCompatibleProvider::new(descriptor, base, Arc::new(HttpClient::new()))
        .with_api_key("secret-key");

    provider.list_models().await.expect("models");
    provider
        .chat(chat_request(OPENAI.chat_model))
        .await
        .expect("chat");
    let requests = server.requests();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|r| r.authorization.as_deref() == Some("Bearer secret-key")),
        "every request must carry the key"
    );

    // Without a key no Authorization header is sent.
    let plain = provider_at(&OPENAI, server.url("/v1"));
    plain.list_models().await.expect("models");
    assert_eq!(
        server
            .requests()
            .last()
            .and_then(|r| r.authorization.clone()),
        None
    );
}

#[tokio::test]
async fn openai_compatible_reports_fixed_capabilities_when_configured() {
    use jarvis_ai::{
        DataLocality, HttpClient, ModelCapabilities, OpenAiCompatibleProvider, ProviderDescriptor,
        ProviderKind,
    };

    let server = MockServer::start(openai_handler(Scenario::default())).await;
    let base = url::Url::parse(&server.url("/v1")).expect("url");
    let descriptor = ProviderDescriptor {
        id: ProviderId::new("runtime-embeddings"),
        display_name: "Runtime embeddings".into(),
        kind: ProviderKind::Embedded,
        locality: DataLocality::OnDevice,
    };
    let embeddings_only = ModelCapabilities {
        embeddings: true,
        ..ModelCapabilities::default()
    };
    let provider = OpenAiCompatibleProvider::new(descriptor, base, Arc::new(HttpClient::new()))
        .with_capabilities(embeddings_only);

    let models = provider.list_models().await.expect("models");
    assert!(!models.is_empty());
    // The id guess would call the chat model a chat model; the fixed value wins.
    assert!(models.iter().all(|m| m.capabilities == embeddings_only));
}
