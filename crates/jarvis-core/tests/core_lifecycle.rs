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
    EchoProvider, FinishReason, ModelCapabilities, ModelInfo, ProviderBackend, ProviderConfig,
    ProviderDescriptor, ProviderFactory, ProviderId, ProviderKind, classify_locality,
    parse_base_url,
};
use jarvis_core::{
    AppPaths, ChatStartRequest, ChatStreamEvent, CoreOptions, ErrorCode, HealthStatus, JarvisCore,
    ModelRef, SinkClosed, TaskCategory,
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
    let core = JarvisCore::initialize(
        CoreOptions::new(paths.clone(), "9.9.9")
            .with_static_providers(providers)
            .with_provider_factory(None),
    )
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
    assert_eq!(report.components.len(), 6);
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

    let first = JarvisCore::initialize(
        CoreOptions::new(paths.clone(), "1.0.0").with_provider_factory(None),
    )
    .await
    .expect("first start");
    let mut settings = first.settings();
    settings.ai.idle_timeout_secs = 42;
    settings.interface.reduced_motion = true;
    first.update_settings(settings.clone()).await.expect("save");
    first.shutdown().await;
    drop(first);

    let second =
        JarvisCore::initialize(CoreOptions::new(paths, "1.0.0").with_provider_factory(None))
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
    test.core.shutdown().await;

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

    let listing = test.core.list_models(false).await;
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

/// Builds [`FakeProvider`]s from configuration, deriving locality from the
/// URL like the real factory. Never touches the network.
#[derive(Debug)]
struct FakeFactory;

impl ProviderFactory for FakeFactory {
    fn create(&self, config: &ProviderConfig) -> Result<Arc<dyn AiProvider>, AiError> {
        let url = parse_base_url(&config.base_url).map_err(AiError::InvalidRequest)?;
        let locality = classify_locality(&url);
        let kind = if locality == DataLocality::ThirdPartyCloud {
            ProviderKind::Cloud
        } else {
            ProviderKind::LocalServer
        };
        let mut provider = FakeProvider::new(config.id.as_str(), kind, locality);
        provider.descriptor.display_name = config.display_name.clone();
        Ok(Arc::new(provider))
    }
}

async fn start_with_factory() -> TestCore {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = paths_in(&dir);
    let core = JarvisCore::initialize(
        CoreOptions::new(paths.clone(), "9.9.9").with_provider_factory(Some(Arc::new(FakeFactory))),
    )
    .await
    .expect("core initializes");
    TestCore {
        core,
        paths,
        _dir: dir,
    }
}

fn provider_ids(listing: &[jarvis_core::ProviderModels]) -> Vec<String> {
    listing.iter().map(|p| p.provider.id.to_string()).collect()
}

#[tokio::test]
async fn configured_providers_follow_settings() {
    let test = start_with_factory().await;
    let listing = test.core.list_models(false).await;
    assert_eq!(
        provider_ids(&listing),
        vec![EchoProvider::ID, "ollama", "lm-studio"],
        "defaults: echo + Ollama + LM Studio"
    );

    let mut settings = test.core.settings();
    settings.ai.providers[1].enabled = false;
    settings.ai.providers.push(ProviderConfig {
        id: ProviderId::new("gpu-box"),
        backend: ProviderBackend::OpenAiCompatible,
        display_name: "GPU box".into(),
        base_url: "http://192.168.1.50:8000/v1".into(),
        enabled: true,
    });
    test.core.update_settings(settings).await.expect("save");

    let listing = test.core.list_models(false).await;
    assert_eq!(
        provider_ids(&listing),
        vec![EchoProvider::ID, "ollama", "gpu-box"],
        "the model cache must be invalidated when providers change"
    );
    let gpu = listing
        .iter()
        .find(|p| p.provider.id.as_str() == "gpu-box")
        .expect("gpu");
    assert_eq!(gpu.provider.locality, DataLocality::LocalNetwork);
}

#[tokio::test]
async fn routing_uses_explicit_choices_and_reports_resolutions() {
    let test = start_with_factory().await;
    let routes = test.core.resolve_routes().await;
    let general = routes
        .iter()
        .find(|r| r.category == TaskCategory::General)
        .expect("general");
    assert!(general.automatic);
    assert_eq!(
        general.provider_id.as_ref().map(ProviderId::as_str),
        Some("ollama")
    );

    let mut settings = test.core.settings();
    settings.ai.routing.general = Some(ModelRef {
        provider_id: ProviderId::new("lm-studio"),
        model: "model".into(),
    });
    test.core.update_settings(settings).await.expect("save");

    let (sink, rx) = channel_sink();
    test.core
        .chat()
        .start(user_request("hi"), sink)
        .await
        .expect("start");
    let events = collect(rx).await;
    match &events[0] {
        ChatStreamEvent::Started { provider_id, .. } => {
            assert_eq!(provider_id.as_str(), "lm-studio")
        }
        other => panic!("expected started, got {other:?}"),
    }
}

#[tokio::test]
async fn provider_connection_test_reports_models_and_locality() {
    let test = start_with_factory().await;
    let result = test
        .core
        .test_provider(ProviderConfig {
            id: ProviderId::new("candidate"),
            backend: ProviderBackend::Ollama,
            display_name: "Candidate".into(),
            base_url: "http://127.0.0.1:11434".into(),
            enabled: true,
        })
        .await
        .expect("test");
    assert!(result.reachable);
    assert_eq!(result.locality, DataLocality::OnDevice);
    assert_eq!(result.models.len(), 1);

    let invalid = test
        .core
        .test_provider(ProviderConfig {
            id: ProviderId::new("candidate"),
            backend: ProviderBackend::Ollama,
            display_name: "Candidate".into(),
            base_url: "http://user:pw@127.0.0.1".into(),
            enabled: true,
        })
        .await
        .expect_err("credentials in URL");
    assert_eq!(invalid.code(), ErrorCode::Validation);
}

// ---------------------------------------------------------------------------
// Embedded runtime
// ---------------------------------------------------------------------------

/// Hosts the fake llama-server (see `jarvis_runtime::testing`).
#[test]
fn fake_llama_server() {
    jarvis_runtime::testing::fake_server_entry();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_served_by_the_runtime_is_routed_and_unregistered_on_stop() {
    use jarvis_runtime::testing;

    let dir = tempfile::tempdir().expect("tempdir");
    let paths = paths_in(&dir);
    testing::install_fake_runtime(&paths.data_dir, None)
        .await
        .expect("fake runtime");
    let core = JarvisCore::initialize(
        CoreOptions::new(paths.clone(), "9.9.9")
            .with_provider_factory(None)
            .with_runtime_command_factory(testing::fake_command_factory(testing::mode::SLOW)),
    )
    .await
    .expect("core");

    let model_path = dir.path().join("tiny.gguf");
    std::fs::write(&model_path, testing::fake_gguf_bytes()).expect("model");
    let model = core
        .runtime()
        .import_model(&model_path)
        .await
        .expect("import");

    core.start_local_model(&model.id).await.expect("start");
    let state = core.runtime().wait_settled(Duration::from_secs(30)).await;
    assert_eq!(
        state.phase,
        jarvis_runtime::ServerPhase::Running,
        "{state:?}"
    );

    // The registry is rebuilt asynchronously after the state change.
    let mut listed = false;
    for _ in 0..50 {
        let listing = core.list_models(true).await;
        if listing.iter().any(|p| {
            p.provider.id.as_str() == jarvis_runtime::RUNTIME_PROVIDER_ID && p.error.is_none()
        }) {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(listed, "runtime provider must appear in the catalog");

    let runtime_component = core
        .health()
        .await
        .components
        .into_iter()
        .find(|c| c.id == "runtime")
        .expect("runtime health");
    assert!(
        runtime_component.message.contains(&model.id),
        "{runtime_component:?}"
    );

    // Automatic routing prefers the embedded runtime over the diagnostic provider.
    let (sink, rx) = channel_sink();
    core.chat()
        .start(user_request("ping"), sink)
        .await
        .expect("chat");
    let events = collect(rx).await;
    match &events[0] {
        ChatStreamEvent::Started {
            provider_id,
            model: routed,
            ..
        } => {
            assert_eq!(provider_id.as_str(), jarvis_runtime::RUNTIME_PROVIDER_ID);
            assert_eq!(routed, &model.id);
        }
        other => panic!("expected started, got {other:?}"),
    }
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ChatStreamEvent::Delta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "pong");

    core.shutdown().await;
    assert!(core.runtime().provider().is_none());
    let mut removed = false;
    for _ in 0..50 {
        if !core
            .list_models(true)
            .await
            .iter()
            .any(|p| p.provider.id.as_str() == jarvis_runtime::RUNTIME_PROVIDER_ID)
        {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(removed, "runtime provider must be unregistered after stop");
}

#[tokio::test]
async fn runtime_settings_are_validated_and_persisted() {
    let test = start_core(vec![Arc::new(EchoProvider::new())]).await;
    let mut settings = test.core.settings();
    settings.runtime.context_size = 100;
    let err = test
        .core
        .update_settings(settings.clone())
        .await
        .expect_err("too small");
    assert_eq!(err.code(), ErrorCode::Validation);

    settings.runtime.context_size = 8192;
    settings.runtime.threads = Some(6);
    settings.runtime.autostart_model = Some("qwen2.5-0.5b-instruct-q4_k_m".into());
    test.core
        .update_settings(settings.clone())
        .await
        .expect("valid");
    assert_eq!(test.core.settings().runtime, settings.runtime);

    // The runtime provider id is reserved for configured providers.
    let mut reserved = test.core.settings();
    reserved.ai.providers[0].id = ProviderId::new(jarvis_runtime::RUNTIME_PROVIDER_ID);
    assert_eq!(
        test.core
            .update_settings(reserved)
            .await
            .expect_err("reserved")
            .code(),
        ErrorCode::Validation
    );
}
