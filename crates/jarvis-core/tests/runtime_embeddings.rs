//! Semantic memory with the embedding server of the embedded runtime.
//!
//! The test binary doubles as a fake `llama-server` (see
//! `jarvis_runtime::testing`); its embedding vectors put words of one
//! meaning ("cat", "kitten") into the same dimension, so a semantic match can
//! be told apart from a word match.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatRequest, ChatStream, DataLocality, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId, ProviderKind,
};
use jarvis_core::jarvis_memory::{
    EmbeddingState, FactInput, MemoryKind, MemoryStatus, SearchHit, SearchQuery,
};
use jarvis_core::{
    AppPaths, ChatStreamEvent, ChatTurnRequest, CoreOptions, JarvisCore, SinkClosed, TaskCategory,
};
use jarvis_runtime::{RUNTIME_EMBEDDINGS_PROVIDER_ID, ServerPhase, ServerRole, testing};
use tokio::sync::mpsc;

/// Hosts the fake llama-server.
#[test]
fn fake_llama_server() {
    testing::fake_server_entry();
}

/// A local chat model that records the system prompts it receives.
#[derive(Debug)]
struct Recorder {
    descriptor: ProviderDescriptor,
    prompts: Mutex<Vec<String>>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new("recorder"),
                display_name: "Recorder".into(),
                kind: ProviderKind::LocalServer,
                locality: DataLocality::OnDevice,
            },
            prompts: Mutex::default(),
        })
    }

    fn last_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl AiProvider for Recorder {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![ModelInfo {
            id: "recorder-model".into(),
            display_name: "Recorder model".into(),
            provider_id: self.descriptor.id.clone(),
            context_window: Some(8192),
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                ..ModelCapabilities::default()
            },
        }])
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        self.prompts
            .lock()
            .unwrap()
            .push(request.messages[0].content.clone());
        Ok(stream::iter([
            Ok(ChatChunk::Delta("Noted.".into())),
            Ok(ChatChunk::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            }),
        ])
        .boxed())
    }
}

fn fact(content: &str) -> FactInput {
    FactInput {
        id: None,
        content: content.to_owned(),
        project_id: None,
        pinned: false,
        source_conversation_id: None,
    }
}

async fn search(core: &JarvisCore, text: &str) -> Vec<SearchHit> {
    core.search_memory(SearchQuery {
        text: text.to_owned(),
        kinds: vec![MemoryKind::Fact],
        limit: 10,
    })
    .await
    .expect("search")
}

/// Polls the memory status until `done` holds (the indexer runs in the
/// background after the providers change).
async fn wait_status(core: &JarvisCore, done: impl Fn(&MemoryStatus) -> bool) -> MemoryStatus {
    for _ in 0..200 {
        let status = core.memory_status().await;
        if done(&status) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    core.memory_status().await
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_search_becomes_semantic_while_the_embedding_server_runs() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    testing::install_fake_runtime(&paths.data_dir, None)
        .await
        .unwrap();
    let recorder = Recorder::new();
    let core = JarvisCore::initialize(
        CoreOptions::new(paths, "9.9.9")
            .with_static_providers(vec![recorder.clone()])
            .with_provider_factory(None)
            .with_runtime_command_factory(testing::fake_command_factory(testing::mode::SLOW)),
    )
    .await
    .unwrap();

    core.save_fact(fact("My cat is called Murzik"))
        .await
        .unwrap();
    core.save_fact(fact("I drive an old car to work"))
        .await
        .unwrap();

    // Without an embedding model the search is text-only: no word matches.
    let status = core.memory_status().await;
    assert_eq!(status.embedding.state, EmbeddingState::Unavailable);
    assert!(search(&core, "kitten").await.is_empty());

    let model_path = dir.path().join("embed.gguf");
    std::fs::write(&model_path, testing::fake_embedding_gguf_bytes()).unwrap();
    let model = core.runtime().import_model(&model_path).await.unwrap();
    core.start_local_model(ServerRole::Embeddings, &model.id)
        .await
        .expect("start");
    let state = core
        .runtime()
        .wait_settled(ServerRole::Embeddings, Duration::from_secs(30))
        .await;
    assert_eq!(state.phase, ServerPhase::Running, "{state:?}");

    // The indexer picks the new embedding model up without being asked.
    let status = wait_status(&core, |s| {
        s.embedding.state == EmbeddingState::Ready && s.embedding.indexed >= 2
    })
    .await;
    assert_eq!(status.embedding.state, EmbeddingState::Ready, "{status:?}");
    assert_eq!(
        status.embedding.model.as_deref(),
        Some(format!("{RUNTIME_EMBEDDINGS_PROVIDER_ID}/{}", model.id).as_str())
    );
    assert_eq!(status.embedding.pending, 0);
    assert_eq!(status.embedding.detail, None);

    // "kitten" now finds the cat by meaning, not by words; the car stays out.
    let hits = search(&core, "kitten").await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].snippet.contains("Murzik"));
    assert!(hits[0].matched_semantic && !hits[0].matched_text);

    // Chat stays on the chat model, and the fact reaches it by meaning.
    let conversation = core.create_conversation(None, false).await.unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    core.send_message(
        ChatTurnRequest {
            conversation_id: conversation.id,
            content: "What is the name of my kitten?".into(),
            provider_id: None,
            model: None,
            temperature: None,
            max_output_tokens: None,
            speak: false,
        },
        move |event| tx.send(event).map_err(|_| SinkClosed),
    )
    .await
    .expect("send");
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    match &events[0] {
        ChatStreamEvent::Started { provider_id, .. } => {
            assert_eq!(provider_id.as_str(), "recorder");
        }
        other => panic!("expected started, got {other:?}"),
    }
    assert!(
        recorder.last_prompt().contains("Murzik"),
        "{}",
        recorder.last_prompt()
    );
    assert!(!recorder.last_prompt().contains("old car"));

    let routes = core.resolve_routes().await;
    let route = |category| {
        routes
            .iter()
            .find(|r| r.category == category)
            .and_then(|r| r.provider_id.clone())
            .map(|id| id.to_string())
    };
    assert_eq!(route(TaskCategory::General).as_deref(), Some("recorder"));
    assert_eq!(
        route(TaskCategory::Embeddings).as_deref(),
        Some(RUNTIME_EMBEDDINGS_PROVIDER_ID)
    );
    let runtime_health = core
        .health()
        .await
        .components
        .into_iter()
        .find(|c| c.id == "runtime")
        .unwrap();
    assert!(
        runtime_health.message.contains("for embeddings"),
        "{runtime_health:?}"
    );

    // Stopping the embedding server makes the search text-only again.
    core.runtime().stop(ServerRole::Embeddings).await;
    let status = wait_status(&core, |s| s.embedding.state == EmbeddingState::Unavailable).await;
    assert_eq!(status.embedding.state, EmbeddingState::Unavailable);
    assert!(
        status
            .embedding
            .detail
            .unwrap_or_default()
            .contains("Runtime"),
        "the detail points at the embedding model of the runtime"
    );
    assert!(search(&core, "kitten").await.is_empty());

    core.shutdown().await;
}
