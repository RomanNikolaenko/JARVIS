//! Integration tests of the public `jarvis-core` API with real files and
//! fake AI providers.

// Test helpers outside `#[test]` functions may fail fast.
#![allow(clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
    assert_eq!(report.components.len(), 7);
    let memory = report
        .components
        .iter()
        .find(|c| c.id == "memory")
        .expect("memory component");
    assert_eq!(memory.status, HealthStatus::Healthy, "{}", memory.message);
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
    fn create(
        &self,
        config: &ProviderConfig,
        _api_key: Option<&str>,
    ) -> Result<Arc<dyn AiProvider>, AiError> {
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
        CoreOptions::new(paths.clone(), "9.9.9")
            .with_secrets(Arc::new(jarvis_security::InMemorySecretStore::new()))
            .with_provider_factory(Some(Arc::new(FakeFactory))),
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
        native_tool_models: vec![],
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
            native_tool_models: vec![],
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
            native_tool_models: vec![],
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

    let mut models_changed = core.subscribe_models_changed();
    // Warm the pre-start cache. A model-change notification must invalidate it.
    assert!(
        !core
            .list_models(false)
            .await
            .iter()
            .any(|p| { p.provider.id.as_str() == jarvis_runtime::RUNTIME_PROVIDER_ID })
    );
    let model_path = dir.path().join("tiny.gguf");
    std::fs::write(&model_path, testing::fake_gguf_bytes()).expect("model");
    let model = core
        .runtime()
        .import_model(&model_path)
        .await
        .expect("import");

    core.start_local_model(jarvis_runtime::ServerRole::Chat, &model.id)
        .await
        .expect("start");
    let state = core
        .runtime()
        .wait_settled(jarvis_runtime::ServerRole::Chat, Duration::from_secs(30))
        .await;
    assert_eq!(
        state.phase,
        jarvis_runtime::ServerPhase::Running,
        "{state:?}"
    );

    // State readiness may precede the asynchronous registry rebuild. Clients
    // receive the event only after a regular cached lookup is up to date.
    tokio::time::timeout(Duration::from_secs(10), models_changed.changed())
        .await
        .expect("model notification")
        .expect("core remains live");
    assert!(core.list_models(false).await.iter().any(|p| {
        p.provider.id.as_str() == jarvis_runtime::RUNTIME_PROVIDER_ID && p.error.is_none()
    }));

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
    assert!(core.runtime().providers().is_empty());
    tokio::time::timeout(Duration::from_secs(10), models_changed.changed())
        .await
        .expect("stop notification")
        .expect("core remains live");
    assert!(
        !core
            .list_models(false)
            .await
            .iter()
            .any(|p| { p.provider.id.as_str() == jarvis_runtime::RUNTIME_PROVIDER_ID })
    );
}

#[tokio::test]
async fn model_notifications_follow_saved_routing_and_privacy_changes() {
    let test = start_core(vec![Arc::new(EchoProvider::new())]).await;
    let mut changes = test.core.subscribe_models_changed();
    let mut settings = test.core.settings();
    settings.ai.routing.general = Some(ModelRef {
        provider_id: ProviderId::new("diagnostic-echo"),
        model: "echo".into(),
    });
    test.core
        .update_settings(settings)
        .await
        .expect("route saved");
    assert!(changes.has_changed().expect("core live"));
    changes.borrow_and_update();
    let routes = test.core.resolve_routes().await;
    assert!(
        routes
            .iter()
            .any(|r| r.category == TaskCategory::General && !r.automatic)
    );

    let mut settings = test.core.settings();
    settings.privacy.allow_cloud_providers = true;
    test.core
        .update_settings(settings)
        .await
        .expect("privacy saved");
    assert!(changes.has_changed().expect("core live"));
    changes.borrow_and_update();

    // Re-saving identical settings or changing an unrelated preference does
    // not make every UI model selector reload its catalog.
    let mut settings = test.core.settings();
    test.core
        .update_settings(settings.clone())
        .await
        .expect("same settings");
    assert!(!changes.has_changed().expect("core live"));
    settings.runtime.context_size = 8192;
    test.core
        .update_settings(settings)
        .await
        .expect("runtime preference");
    assert!(!changes.has_changed().expect("core live"));
    test.core.shutdown().await;
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

#[tokio::test]
async fn provider_api_keys_use_the_injected_secret_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = paths_in(&dir);
    let secrets = Arc::new(jarvis_security::InMemorySecretStore::new());
    let core = JarvisCore::initialize(
        CoreOptions::new(paths, "9.9.9")
            .with_provider_factory(None)
            .with_secrets(secrets),
    )
    .await
    .expect("core");

    let provider_id = "test-cloud";
    assert!(!core.has_provider_api_key(provider_id).await.unwrap());

    core.set_provider_api_key(provider_id, "sk-test-12345")
        .await
        .unwrap();
    assert!(core.has_provider_api_key(provider_id).await.unwrap());

    core.delete_provider_api_key(provider_id).await.unwrap();
    assert!(!core.has_provider_api_key(provider_id).await.unwrap());
}

#[derive(Debug)]
struct KeyAwareFactory;

#[derive(Debug)]
struct ThreadCheckedVault(std::thread::ThreadId);

impl jarvis_security::SecretStore for ThreadCheckedVault {
    fn get_secret(&self, _key: &str) -> Result<Option<String>, jarvis_security::SecretError> {
        assert_ne!(
            std::thread::current().id(),
            self.0,
            "vault blocked the async executor"
        );
        Ok(None)
    }

    fn set_secret(&self, key: &str, _secret: &str) -> Result<(), jarvis_security::SecretError> {
        self.get_secret(key).map(|_| ())
    }

    fn delete_secret(&self, key: &str) -> Result<(), jarvis_security::SecretError> {
        self.get_secret(key).map(|_| ())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_vault_operations_run_off_the_async_executor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = JarvisCore::initialize(
        CoreOptions::new(paths_in(&dir), "test")
            .with_secrets(Arc::new(ThreadCheckedVault(std::thread::current().id())))
            .with_provider_factory(Some(Arc::new(KeyAwareFactory))),
    )
    .await
    .expect("core initialization reads keys off executor");
    let config = core
        .settings()
        .ai
        .providers
        .into_iter()
        .find(|p| p.enabled)
        .expect("provider");
    core.test_provider(config.clone())
        .await
        .expect("connection test");
    core.has_provider_api_key(config.id.as_str())
        .await
        .expect("has key");
    core.set_provider_api_key(config.id.as_str(), "test-key")
        .await
        .expect("set and reload");
    core.delete_provider_api_key(config.id.as_str())
        .await
        .expect("delete and reload");
    core.shutdown().await;
}

#[derive(Debug, Default)]
struct UnavailableVault {
    reads: AtomicUsize,
}

impl jarvis_security::SecretStore for UnavailableVault {
    fn get_secret(&self, _key: &str) -> Result<Option<String>, jarvis_security::SecretError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Err(jarvis_security::SecretError::Unavailable("locked".into()))
    }
    fn set_secret(&self, _key: &str, _secret: &str) -> Result<(), jarvis_security::SecretError> {
        Err(jarvis_security::SecretError::Unavailable("locked".into()))
    }
    fn delete_secret(&self, _key: &str) -> Result<(), jarvis_security::SecretError> {
        Err(jarvis_security::SecretError::Unavailable("locked".into()))
    }
}

#[tokio::test]
async fn unavailable_vault_keeps_local_servers_usable_and_reports_key_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = JarvisCore::initialize(
        CoreOptions::new(paths_in(&dir), "test")
            .with_secrets(Arc::new(UnavailableVault::default()))
            .with_provider_factory(Some(Arc::new(KeyAwareFactory))),
    )
    .await
    .expect("core");
    let models = core.list_models(false).await;
    assert!(models.iter().any(|p| p.provider.id.as_str() == "lm-studio"));
    assert!(
        core.set_provider_api_key("lm-studio", "test-key")
            .await
            .is_err()
    );
    let mut settings = core.settings();
    settings.ai.providers.push(ProviderConfig {
        native_tool_models: vec![],
        id: ProviderId::new("cloud-test"),
        backend: ProviderBackend::Anthropic,
        display_name: "Cloud".into(),
        base_url: "https://example.com/v1".into(),
        enabled: true,
    });
    core.update_settings(settings).await.expect("settings");
    assert!(
        !core
            .list_models(false)
            .await
            .iter()
            .any(|p| p.provider.id.as_str() == "cloud-test")
    );
    core.shutdown().await;
}

#[derive(Debug, Default)]
struct RecordingKeyFactory {
    calls: Mutex<Vec<(ProviderId, Option<String>)>>,
}

impl ProviderFactory for RecordingKeyFactory {
    fn create(
        &self,
        config: &ProviderConfig,
        api_key: Option<&str>,
    ) -> Result<Arc<dyn AiProvider>, AiError> {
        self.calls
            .lock()
            .expect("factory calls")
            .push((config.id.clone(), api_key.map(str::to_owned)));
        FakeFactory.create(config, api_key)
    }
}

#[tokio::test]
async fn connection_tests_keep_local_fallback_and_require_cloud_credentials() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Arc::new(UnavailableVault::default());
    let factory = Arc::new(RecordingKeyFactory::default());
    let core = JarvisCore::initialize(
        CoreOptions::new(paths_in(&dir), "test")
            .with_secrets(vault.clone())
            .with_provider_factory(Some(factory.clone())),
    )
    .await
    .expect("core");
    factory.calls.lock().expect("calls").clear();
    let mut config = ProviderConfig::defaults().remove(1);
    config.id = ProviderId::new("connection-probe");
    config.display_name = "Connection probe".into();
    for (url, locality) in [
        ("http://127.0.0.1:1234/v1", DataLocality::OnDevice),
        ("http://192.168.1.20:1234/v1", DataLocality::LocalNetwork),
    ] {
        config.base_url = url.into();
        let result = core
            .test_provider(config.clone())
            .await
            .expect("local test");
        assert!(result.reachable);
        assert_eq!(result.locality, locality);
        assert_eq!(result.models.len(), 1);
        assert_eq!(
            factory.calls.lock().expect("calls").last(),
            Some(&(config.id.clone(), None))
        );
    }
    assert!(core.events().recent(100).iter().any(|event| {
        event.category == EventCategory::Ai
            && event.level == jarvis_events::EventLevel::Warning
            && event.message.contains("Connection probe")
            && event.message.contains("cannot load its API key")
    }));

    config.base_url = "https://api.example.com/v1".into();
    let calls_before = factory.calls.lock().expect("calls").len();
    let error = core
        .test_provider(config.clone())
        .await
        .expect_err("locked cloud vault");
    assert_eq!(error.code(), ErrorCode::Internal);
    assert!(error.to_string().contains("locked"));
    assert_eq!(factory.calls.lock().expect("calls").len(), calls_before);

    let reads_before = vault.reads.load(Ordering::SeqCst);
    let result = core
        .test_provider_with_key(config.clone(), Some("explicit-test-key".into()))
        .await
        .expect("explicit key bypasses the locked vault");
    assert!(result.reachable);
    assert_eq!(result.locality, DataLocality::ThirdPartyCloud);
    assert_eq!(vault.reads.load(Ordering::SeqCst), reads_before);
    assert_eq!(
        factory.calls.lock().expect("calls").last(),
        Some(&(config.id, Some("explicit-test-key".into())))
    );
    assert!(
        core.events()
            .recent(100)
            .iter()
            .all(|event| !event.message.contains("explicit-test-key"))
    );
    core.shutdown().await;
}

#[tokio::test]
async fn connection_tests_validate_explicit_keys_before_using_vault_or_factory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Arc::new(UnavailableVault::default());
    let factory = Arc::new(RecordingKeyFactory::default());
    let core = JarvisCore::initialize(
        CoreOptions::new(paths_in(&dir), "test")
            .with_secrets(vault.clone())
            .with_provider_factory(Some(factory.clone())),
    )
    .await
    .expect("core");
    factory.calls.lock().expect("calls").clear();
    let reads_before = vault.reads.load(Ordering::SeqCst);
    let config = ProviderConfig::defaults().remove(1);
    for invalid in ["é".repeat(4097), "key\nsuffix".into(), "key\u{7f}".into()] {
        let error = core
            .test_provider_with_key(config.clone(), Some(invalid.clone()))
            .await
            .expect_err("invalid test key");
        assert_eq!(error.code(), ErrorCode::Validation);
        let error = core
            .set_provider_api_key(config.id.as_str(), &invalid)
            .await
            .expect_err("invalid saved key");
        assert_eq!(error.code(), ErrorCode::Validation);
    }
    assert_eq!(vault.reads.load(Ordering::SeqCst), reads_before);
    assert!(factory.calls.lock().expect("calls").is_empty());
    core.shutdown().await;
}

impl ProviderFactory for KeyAwareFactory {
    fn create(
        &self,
        config: &ProviderConfig,
        key: Option<&str>,
    ) -> Result<Arc<dyn AiProvider>, AiError> {
        let mut provider = FakeProvider::new(
            config.id.as_str(),
            ProviderKind::LocalServer,
            DataLocality::OnDevice,
        );
        // A visible marker lets the test check registry replacement and catalog invalidation.
        provider.descriptor.display_name = key.unwrap_or("no-key").to_owned();
        Ok(Arc::new(provider))
    }
}

#[tokio::test]
async fn key_changes_replace_providers_and_invalidate_cached_models() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = JarvisCore::initialize(
        CoreOptions::new(paths_in(&dir), "test")
            .with_secrets(Arc::new(jarvis_security::InMemorySecretStore::new()))
            .with_provider_factory(Some(Arc::new(KeyAwareFactory))),
    )
    .await
    .expect("core");
    for key in [Some("first"), Some("rotated"), None] {
        // Populate the cache before each change.
        core.list_models(false).await;
        if let Some(key) = key {
            core.set_provider_api_key("lm-studio", key)
                .await
                .expect("set");
        } else {
            core.delete_provider_api_key("lm-studio")
                .await
                .expect("delete");
        }
        let models = core.list_models(false).await;
        let provider = models
            .iter()
            .find(|p| p.provider.id.as_str() == "lm-studio")
            .expect("provider");
        assert_eq!(provider.provider.display_name, key.unwrap_or("no-key"));
    }
    assert!(core.set_provider_api_key("bad id", "key").await.is_err());
    assert!(core.set_provider_api_key("lm-studio", " ").await.is_err());
    core.shutdown().await;
}
