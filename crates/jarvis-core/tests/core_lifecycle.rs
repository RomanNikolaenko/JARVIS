//! Integration tests of the public `jarvis-core` API with real files and
//! fake AI providers.

// Test helpers outside `#[test]` functions may fail fast.
#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream;
use jarvis_ai::{
    AiError, AiProvider, ChatChunk, ChatMessage, ChatRequest, ChatStream, DataLocality,
    EchoProvider, FinishReason, ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId,
    ProviderKind,
};
use jarvis_core::{
    AppPaths, ChatStartRequest, ChatStreamEvent, CoreOptions, ErrorCode, HealthStatus, JarvisCore,
    SinkClosed,
};
use jarvis_events::EventCategory;
use tokio::sync::mpsc;

/// Behaviour of [`FakeProvider`] streams.
#[derive(Debug, Clone, Copy)]
enum StreamBehaviour {
    /// Replies "ok" and finishes.
    Reply,
    /// Never yields a chunk.
    Hang,
}

#[derive(Debug)]
struct FakeProvider {
    descriptor: ProviderDescriptor,
    behaviour: StreamBehaviour,
    list_models_fails: bool,
}

impl FakeProvider {
    fn new(id: &str, kind: ProviderKind, locality: DataLocality) -> Self {
        Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::new(id),
                display_name: format!("Fake {id}"),
                kind,
                locality,
            },
            behaviour: StreamBehaviour::Reply,
            list_models_fails: false,
        }
    }
}

#[async_trait]
impl AiProvider for FakeProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn health_check(&self) -> Result<(), AiError> {
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        if self.list_models_fails {
            return Err(AiError::Unavailable("connection refused".into()));
        }
        Ok(vec![ModelInfo {
            id: "model".into(),
            display_name: "Model".into(),
            provider_id: self.descriptor.id.clone(),
            context_window: Some(8192),
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                ..ModelCapabilities::default()
            },
        }])
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<ChatStream, AiError> {
        Ok(match self.behaviour {
            StreamBehaviour::Reply => stream::iter(vec![
                Ok(ChatChunk::Delta("ok".into())),
                Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                }),
            ])
            .boxed(),
            StreamBehaviour::Hang => stream::pending().boxed(),
        })
    }
}

struct TestCore {
    core: Arc<JarvisCore>,
    paths: AppPaths,
    _dir: tempfile::TempDir,
}

fn paths_in(dir: &tempfile::TempDir) -> AppPaths {
    AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    }
}

async fn start_core(providers: Vec<Arc<dyn AiProvider>>) -> TestCore {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = paths_in(&dir);
    let core =
        JarvisCore::initialize(CoreOptions::new(paths.clone(), "9.9.9").with_providers(providers))
            .await
            .expect("core initializes");
    TestCore {
        core,
        paths,
        _dir: dir,
    }
}

fn user_request(text: &str) -> ChatStartRequest {
    ChatStartRequest {
        provider_id: None,
        model: None,
        messages: vec![ChatMessage::user(text)],
        temperature: None,
        max_output_tokens: None,
    }
}

fn channel_sink() -> (
    impl FnMut(ChatStreamEvent) -> Result<(), SinkClosed> + Send + 'static,
    mpsc::UnboundedReceiver<ChatStreamEvent>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    (move |event| tx.send(event).map_err(|_| SinkClosed), rx)
}

async fn collect(mut rx: mpsc::UnboundedReceiver<ChatStreamEvent>) -> Vec<ChatStreamEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn initialization_creates_directories_and_reports_health() {
    let test = start_core(vec![Arc::new(EchoProvider::new())]).await;

    assert!(test.paths.config_file().is_file(), "settings file created");
    assert!(test.paths.data_dir.is_dir());
    assert!(test.paths.log_dir.is_dir());

    let info = test.core.info();
    assert_eq!(info.version, "9.9.9");
    assert!(info.config_file.ends_with("config.toml"));

    let report = test.core.health().await;
    assert_eq!(report.components.len(), 5);
    let ai = report
        .components
        .iter()
        .find(|c| c.id == "ai")
        .expect("ai component");
    assert_eq!(ai.status, HealthStatus::Degraded, "{}", ai.message);
    assert_eq!(report.status, HealthStatus::Degraded);
}

#[tokio::test]
async fn real_providers_make_ai_healthy() {
    let test = start_core(vec![Arc::new(FakeProvider::new(
        "local",
        ProviderKind::LocalServer,
        DataLocality::OnDevice,
    ))])
    .await;
    let report = test.core.health().await;
    assert_eq!(report.status, HealthStatus::Healthy, "{report:?}");
}

#[tokio::test]
async fn settings_persist_across_restarts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = paths_in(&dir);

    let first = JarvisCore::initialize(CoreOptions::new(paths.clone(), "1.0.0"))
        .await
        .expect("first start");
    let mut settings = first.settings();
    settings.ai.idle_timeout_secs = 42;
    settings.interface.reduced_motion = true;
    first.update_settings(settings.clone()).await.expect("save");
    first.shutdown();
    drop(first);

    let second = JarvisCore::initialize(CoreOptions::new(paths, "1.0.0"))
        .await
        .expect("second start");
    assert_eq!(second.settings(), settings);
}

#[tokio::test]
async fn cloud_providers_are_blocked_until_the_user_allows_them() {
    let test = start_core(vec![Arc::new(FakeProvider::new(
        "cloud",
        ProviderKind::Cloud,
        DataLocality::ThirdPartyCloud,
    ))])
    .await;

    let (sink, _rx) = channel_sink();
    let err = test
        .core
        .chat()
        .start(user_request("secret"), sink)
        .await
        .expect_err("cloud must be blocked by default");
    assert_eq!(err.code(), ErrorCode::PermissionDenied);
    assert!(
        test.core
            .events()
            .recent(50)
            .iter()
            .any(|e| e.category == EventCategory::Security),
        "blocked request must be recorded as a security event"
    );

    let mut settings = test.core.settings();
    settings.privacy.allow_cloud_providers = true;
    test.core.update_settings(settings).await.expect("save");

    let (sink, rx) = channel_sink();
    test.core
        .chat()
        .start(user_request("hello"), sink)
        .await
        .expect("allowed after opt-in");
    let events = collect(rx).await;
    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Finished {
            finish_reason: FinishReason::Stop,
            ..
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn idle_timeout_fails_streams_that_stop_producing() {
    let mut provider = FakeProvider::new("slow", ProviderKind::LocalServer, DataLocality::OnDevice);
    provider.behaviour = StreamBehaviour::Hang;
    let test = start_core(vec![Arc::new(provider)]).await;

    let mut settings = test.core.settings();
    settings.ai.idle_timeout_secs = 5;
    test.core.update_settings(settings).await.expect("save");

    let (sink, rx) = channel_sink();
    test.core
        .chat()
        .start(user_request("hello"), sink)
        .await
        .expect("start");
    let events = collect(rx).await;
    match events.last() {
        Some(ChatStreamEvent::Failed { error, .. }) => assert_eq!(error.code, ErrorCode::Timeout),
        other => panic!("expected a timeout failure, got {other:?}"),
    }
}

#[tokio::test]
async fn shutdown_cancels_running_chats() {
    let test = start_core(vec![Arc::new(EchoProvider::with_chunk_delay(
        Duration::from_secs(30),
    ))])
    .await;

    let (sink, rx) = channel_sink();
    test.core
        .chat()
        .start(user_request("a long answer"), sink)
        .await
        .expect("start");
    test.core.shutdown();

    let events = collect(rx).await;
    assert!(matches!(
        events.last(),
        Some(ChatStreamEvent::Finished {
            finish_reason: FinishReason::Cancelled,
            ..
        })
    ));
}

#[tokio::test]
async fn model_listing_reports_failures_per_provider() {
    let mut broken = FakeProvider::new("broken", ProviderKind::LocalServer, DataLocality::OnDevice);
    broken.list_models_fails = true;
    let test = start_core(vec![Arc::new(broken), Arc::new(EchoProvider::new())]).await;

    let listing = test.core.list_models().await;
    assert_eq!(listing.len(), 2);
    let broken = listing
        .iter()
        .find(|p| p.provider.id.as_str() == "broken")
        .expect("broken provider listed");
    assert!(broken.models.is_empty());
    assert!(
        broken
            .error
            .as_deref()
            .unwrap_or("")
            .contains("connection refused")
    );
    let echo = listing
        .iter()
        .find(|p| p.provider.id.as_str() == EchoProvider::ID)
        .expect("echo listed");
    assert_eq!(echo.models.len(), 1);
    assert!(echo.error.is_none());
}

#[tokio::test]
async fn system_metrics_initialize_lazily_and_concurrently() {
    let test = start_core(vec![Arc::new(EchoProvider::new())]).await;
    let (a, b) = tokio::join!(test.core.system_metrics(), test.core.system_metrics());
    let (a, b) = (a.expect("metrics"), b.expect("metrics"));
    assert!(a.memory_total_bytes > 0);
    assert_eq!(a.memory_total_bytes, b.memory_total_bytes);
}
