//! Lifecycle tests of the supervised servers with real processes.
//!
//! The test binary doubles as a fake `llama-server`: when started with the
//! `JARVIS_FAKE_LLAMA` environment variable, the `fake_llama_server` test
//! serves a minimal llama.cpp-like HTTP API instead of returning.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jarvis_ai::{ChatMessage, ChatRequest, EmbeddingRequest, GenerationOptions, HttpClient};
use jarvis_events::{EventBus, EventCategory, EventLevel};
use jarvis_runtime::process::{CommandFactory, LaunchPlan};
use jarvis_runtime::testing::{self, FAKE_ENV};
use jarvis_runtime::{
    RUNTIME_EMBEDDINGS_PROVIDER_ID, RUNTIME_PROVIDER_ID, RuntimeError, RuntimeManager,
    RuntimeOptions, RuntimeSettings, ServerPhase, ServerRole,
};

const CHAT: ServerRole = ServerRole::Chat;
const EMBEDDINGS: ServerRole = ServerRole::Embeddings;

#[test]
fn fake_llama_server() {
    testing::fake_server_entry();
}

fn fake_command(mode: &'static str) -> CommandFactory {
    testing::fake_command_factory(mode)
}

/// Chat servers in `chat`, embedding servers in `embeddings` mode.
fn fake_command_by_role(chat: &'static str, embeddings: &'static str) -> CommandFactory {
    let (chat, embeddings) = (fake_command(chat), fake_command(embeddings));
    Arc::new(move |plan: &LaunchPlan| {
        if plan.embeddings.is_some() {
            embeddings(plan)
        } else {
            chat(plan)
        }
    })
}

struct Harness {
    manager: Arc<RuntimeManager>,
    events: EventBus,
    /// A chat model (no pooling metadata).
    model_id: String,
    /// An embedding model (CLS pooling).
    embedding_model_id: String,
    data_dir: PathBuf,
    /// The fake `llama-server` (a copy of this test binary).
    engine: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn import(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.data_dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        self.manager.import_model(&path).await.unwrap().id
    }

    fn pid_file(&self, name: &str) -> PathBuf {
        self.data_dir.join("runtime").join(name)
    }
}

async fn harness(mode: &'static str) -> Harness {
    harness_with(fake_command(mode)).await
}

async fn harness_with(command_factory: CommandFactory) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let engine = testing::install_fake_runtime(&data_dir, None)
        .await
        .unwrap();

    let events = EventBus::default();
    let manager = RuntimeManager::new(RuntimeOptions {
        data_dir: data_dir.clone(),
        events: events.clone(),
        ai_http: Arc::new(HttpClient::new()),
        command_factory: Some(command_factory),
    })
    .await
    .unwrap();

    let mut h = Harness {
        manager,
        events,
        model_id: String::new(),
        embedding_model_id: String::new(),
        data_dir,
        engine,
        _dir: dir,
    };
    h.model_id = h
        .import("fake-model.gguf", &testing::fake_gguf_bytes())
        .await;
    h.embedding_model_id = h
        .import("fake-embed.gguf", &testing::fake_embedding_gguf_bytes())
        .await;
    h
}

fn settings(timeout_secs: u32) -> RuntimeSettings {
    RuntimeSettings {
        context_size: 512,
        startup_timeout_secs: timeout_secs,
        ..RuntimeSettings::default()
    }
}

async fn wait_gone(pid: u32) -> bool {
    for _ in 0..50 {
        if jarvis_platform::process_memory(pid).is_none() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn native_tools_follow_exact_model_selection_and_reset_on_restart() {
    let plans = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = plans.clone();
    let factory = fake_command("slow");
    let h = harness_with(Arc::new(move |plan| {
        observed.lock().unwrap().push(plan.clone());
        factory(plan)
    }))
    .await;
    let mut config = settings(30);
    for (ids, enabled) in [
        (vec![format!("{}-other", h.model_id)], false),
        (vec![h.model_id.clone()], true),
        (Vec::new(), false),
    ] {
        config.native_tool_models = ids;
        h.manager.start(CHAT, &h.model_id, &config).await.unwrap();
        let state = h.manager.wait_settled(CHAT, Duration::from_secs(30)).await;
        assert_eq!(state.phase, ServerPhase::Running, "{state:?}");
        assert_eq!(state.tool_calling, enabled);
        assert_eq!(plans.lock().unwrap().last().unwrap().native_tools, enabled);
        let provider = h.manager.provider(CHAT).unwrap();
        assert!(provider.supports_native_tools());
        assert_eq!(
            provider.list_models().await.unwrap()[0]
                .capabilities
                .tool_calling,
            enabled
        );
    }
    config.native_tool_models = vec![h.embedding_model_id.clone()];
    h.manager
        .start(EMBEDDINGS, &h.embedding_model_id, &config)
        .await
        .unwrap();
    let state = h
        .manager
        .wait_settled(EMBEDDINGS, Duration::from_secs(30))
        .await;
    assert_eq!(state.phase, ServerPhase::Running, "{state:?}");
    assert!(!state.tool_calling);
    assert!(!plans.lock().unwrap().last().unwrap().native_tools);
    assert!(
        !h.manager
            .provider(EMBEDDINGS)
            .unwrap()
            .list_models()
            .await
            .unwrap()[0]
            .capabilities
            .tool_calling
    );
    h.manager.stop(CHAT).await;
    h.manager.stop(EMBEDDINGS).await;
    assert!(!h.manager.server_state(CHAT).tool_calling);
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_serves_with_the_api_key_and_stops() {
    let h = harness("slow").await;
    assert!(h.manager.provider(CHAT).is_none());
    h.manager
        .start(CHAT, &h.model_id, &settings(30))
        .await
        .expect("start");
    assert_eq!(h.manager.server_state(CHAT).phase, ServerPhase::Starting);

    let state = h.manager.wait_settled(CHAT, Duration::from_secs(30)).await;
    assert_eq!(state.phase, ServerPhase::Running, "{state:?}");
    let pid = state.pid.expect("pid");
    assert!(h.pid_file("server.pid").exists());

    let provider = h
        .manager
        .provider(CHAT)
        .expect("runtime provider registered");
    assert_eq!(provider.descriptor().id.as_str(), RUNTIME_PROVIDER_ID);
    let models = provider
        .list_models()
        .await
        .expect("models with the API key");
    assert_eq!(models[0].id, h.model_id);
    assert!(models[0].capabilities.chat && !models[0].capabilities.embeddings);
    let reply = provider
        .chat(ChatRequest {
            model: h.model_id.clone(),
            messages: vec![ChatMessage::user("ping")],
            options: GenerationOptions::default(),
        })
        .await
        .expect("chat");
    assert_eq!(reply.content, "pong");
    assert!(
        !h.events
            .recent(100)
            .iter()
            .any(|e| e.category == EventCategory::Security),
        "the fake server enforces the key, so no security warning is expected"
    );

    let status = h.manager.status().await;
    assert!(status.server.memory_bytes.is_some_and(|m| m > 0));
    assert_eq!(status.embedding_server.phase, ServerPhase::Stopped);

    h.manager.stop(CHAT).await;
    assert_eq!(h.manager.server_state(CHAT).phase, ServerPhase::Stopped);
    assert!(h.manager.provider(CHAT).is_none());
    assert!(!h.pid_file("server.pid").exists());
    assert!(wait_gone(pid).await, "server process must be gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_and_embedding_servers_run_side_by_side() {
    let h = harness("slow").await;
    h.manager
        .start(CHAT, &h.model_id, &settings(30))
        .await
        .expect("chat start");
    h.manager
        .start(EMBEDDINGS, &h.embedding_model_id, &settings(30))
        .await
        .expect("embedding start");
    let chat = h.manager.wait_settled(CHAT, Duration::from_secs(30)).await;
    let embedding = h
        .manager
        .wait_settled(EMBEDDINGS, Duration::from_secs(30))
        .await;
    assert_eq!(chat.phase, ServerPhase::Running, "{chat:?}");
    assert_eq!(embedding.phase, ServerPhase::Running, "{embedding:?}");
    assert_ne!(chat.port, embedding.port);
    assert_ne!(chat.pid, embedding.pid);
    assert_eq!(
        embedding.model_id.as_deref(),
        Some(h.embedding_model_id.as_str())
    );
    // The fake model reports no training context: the full embedding batch.
    assert_eq!(
        embedding.context_size,
        Some(jarvis_runtime::EMBEDDING_CONTEXT)
    );
    assert!(h.pid_file("server.pid").exists());
    assert!(h.pid_file("embedding-server.pid").exists());

    let embedder = h.manager.provider(EMBEDDINGS).expect("embedding provider");
    assert_eq!(
        embedder.descriptor().id.as_str(),
        RUNTIME_EMBEDDINGS_PROVIDER_ID
    );
    let models = embedder
        .list_models()
        .await
        .expect("models with the API key");
    assert_eq!(models[0].id, h.embedding_model_id);
    assert!(models[0].capabilities.embeddings && !models[0].capabilities.chat);
    let vectors = embedder
        .embeddings(EmbeddingRequest {
            model: h.embedding_model_id.clone(),
            input: vec!["my cat".into(), "a kitten".into(), "an old car".into()],
        })
        .await
        .expect("embeddings")
        .vectors;
    assert_eq!(vectors.len(), 3);
    assert!(
        vectors
            .iter()
            .all(|v| v.len() == testing::FAKE_EMBEDDING_DIMS)
    );
    assert_eq!(vectors[0], testing::fake_embedding("my cat"));

    // Each server does only its own job.
    let chat_provider = h.manager.provider(CHAT).expect("chat provider");
    assert!(
        chat_provider
            .embeddings(EmbeddingRequest {
                model: h.model_id.clone(),
                input: vec!["x".into()],
            })
            .await
            .is_err(),
        "a chat server is not started with --embedding"
    );
    let ids: Vec<String> = h
        .manager
        .providers()
        .iter()
        .map(|p| p.descriptor().id.to_string())
        .collect();
    assert_eq!(ids, [RUNTIME_PROVIDER_ID, RUNTIME_EMBEDDINGS_PROVIDER_ID]);

    let status = h.manager.status().await;
    assert!(status.embedding_server.memory_bytes.is_some_and(|m| m > 0));
    let err = h
        .manager
        .remove_model(&h.embedding_model_id, false)
        .await
        .expect_err("in use");
    assert!(matches!(err, RuntimeError::InvalidInput(_)), "{err:?}");

    // Stopping one server leaves the other running.
    let embedding_pid = embedding.pid.expect("pid");
    h.manager.stop(EMBEDDINGS).await;
    assert_eq!(
        h.manager.server_state(EMBEDDINGS).phase,
        ServerPhase::Stopped
    );
    assert!(h.manager.provider(EMBEDDINGS).is_none());
    assert!(!h.pid_file("embedding-server.pid").exists());
    assert!(
        wait_gone(embedding_pid).await,
        "embedding server must be gone"
    );
    assert_eq!(h.manager.server_state(CHAT).phase, ServerPhase::Running);
    assert_eq!(h.manager.providers().len(), 1);

    h.manager.shutdown().await;
    assert_eq!(h.manager.server_state(CHAT).phase, ServerPhase::Stopped);
    assert!(h.manager.providers().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_embedding_server_crash_leaves_the_chat_model_running() {
    let h = harness_with(fake_command_by_role("slow", "crash-after-ready")).await;
    h.manager
        .start(CHAT, &h.model_id, &settings(30))
        .await
        .expect("chat start");
    h.manager
        .start(EMBEDDINGS, &h.embedding_model_id, &settings(30))
        .await
        .expect("embedding start");
    assert_eq!(
        h.manager
            .wait_settled(EMBEDDINGS, Duration::from_secs(30))
            .await
            .phase,
        ServerPhase::Running
    );

    let mut state = h.manager.subscribe_state(EMBEDDINGS);
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| s.phase == ServerPhase::Failed),
    )
    .await
    .expect("crash detected")
    .expect("state channel");
    assert!(h.manager.provider(EMBEDDINGS).is_none());
    assert!(!h.pid_file("embedding-server.pid").exists());
    assert_eq!(
        h.manager
            .wait_settled(CHAT, Duration::from_secs(30))
            .await
            .phase,
        ServerPhase::Running
    );
    assert!(h.manager.provider(CHAT).is_some());
    assert!(h.events.recent(50).iter().any(|e| {
        e.level == EventLevel::Error && e.message.contains("for embeddings: llama-server exited")
    }));
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn embedding_pooling_comes_from_the_model() {
    let h = harness("slow").await;
    let reranker = h
        .import("fake-rerank.gguf", &testing::fake_gguf(Some(4)))
        .await;
    let err = h
        .manager
        .start(EMBEDDINGS, &reranker, &settings(30))
        .await
        .expect_err("rerankers produce no embeddings");
    assert!(matches!(err, RuntimeError::InvalidInput(_)), "{err:?}");
    assert_eq!(
        h.manager.server_state(EMBEDDINGS).phase,
        ServerPhase::Stopped
    );

    // A model without pooling metadata runs with mean pooling and a warning.
    h.manager
        .start(EMBEDDINGS, &h.model_id, &settings(30))
        .await
        .expect("start");
    assert_eq!(
        h.manager
            .wait_settled(EMBEDDINGS, Duration::from_secs(30))
            .await
            .phase,
        ServerPhase::Running
    );
    assert!(
        h.events
            .recent(50)
            .iter()
            .any(|e| e.level == EventLevel::Warning && e.message.contains("mean pooling"))
    );
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_failures_report_the_server_log() {
    let h = harness("exit-early").await;
    for (role, model) in [(CHAT, &h.model_id), (EMBEDDINGS, &h.embedding_model_id)] {
        h.manager
            .start(role, model, &settings(30))
            .await
            .expect("spawned");
        let state = h.manager.wait_settled(role, Duration::from_secs(30)).await;
        assert_eq!(state.phase, ServerPhase::Failed, "{role:?}");
        let error = state.error.expect("error");
        assert!(error.contains("fake failure"), "{error}");
        assert!(h.manager.provider(role).is_none());
    }
    let status = h.manager.status().await;
    assert!(
        status
            .embedding_log_tail
            .iter()
            .any(|l| l.contains("fake failure"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn crashes_after_startup_are_detected() {
    let h = harness("crash-after-ready").await;
    h.manager
        .start(CHAT, &h.model_id, &settings(30))
        .await
        .expect("start");
    assert_eq!(
        h.manager
            .wait_settled(CHAT, Duration::from_secs(30))
            .await
            .phase,
        ServerPhase::Running
    );

    let mut state = h.manager.subscribe_state(CHAT);
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| s.phase == ServerPhase::Failed),
    )
    .await
    .expect("crash detected")
    .expect("state channel");
    assert!(
        h.manager.provider(CHAT).is_none(),
        "a crashed server is unregistered"
    );
    assert!(
        h.manager
            .server_state(CHAT)
            .error
            .unwrap_or_default()
            .contains("unexpectedly")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_servers_time_out() {
    let h = harness("never-ready").await;
    h.manager
        .start(EMBEDDINGS, &h.embedding_model_id, &settings(2))
        .await
        .expect("start");
    let state = h
        .manager
        .wait_settled(EMBEDDINGS, Duration::from_secs(20))
        .await;
    assert_eq!(state.phase, ServerPhase::Failed);
    assert!(
        state
            .error
            .unwrap_or_default()
            .contains("did not become ready")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_binary_is_refused() {
    let h = harness("slow").await;
    testing::install_fake_runtime(&h.data_dir, Some("00".repeat(32)))
        .await
        .unwrap();
    // Reload the record through a new manager.
    let manager = RuntimeManager::new(RuntimeOptions {
        data_dir: h.data_dir.clone(),
        events: EventBus::default(),
        ai_http: Arc::new(HttpClient::new()),
        command_factory: Some(fake_command("slow")),
    })
    .await
    .unwrap();
    for (role, model) in [(CHAT, &h.model_id), (EMBEDDINGS, &h.embedding_model_id)] {
        let err = manager
            .start(role, model, &settings(30))
            .await
            .expect_err("tampered binary");
        assert!(matches!(err, RuntimeError::BinaryChanged(_)), "{err:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_library_planted_next_to_the_server_is_refused() {
    let h = harness("slow").await;
    // The loader would pick this up from the server's directory.
    let planted = h.engine.with_file_name("version.dll");
    std::fs::write(&planted, b"not a real library").unwrap();
    for (role, model) in [(CHAT, &h.model_id), (EMBEDDINGS, &h.embedding_model_id)] {
        let err = h
            .manager
            .start(role, model, &settings(30))
            .await
            .expect_err("planted library");
        match err {
            RuntimeError::BinaryChanged(path) => assert_eq!(path, planted),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    std::fs::remove_file(&planted).unwrap();
    h.manager
        .start(EMBEDDINGS, &h.embedding_model_id, &settings(30))
        .await
        .expect("starts again once the folder is clean");
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_models_and_missing_runtime_are_reported() {
    let h = harness("slow").await;
    let err = h
        .manager
        .start(CHAT, "missing", &settings(30))
        .await
        .expect_err("unknown model");
    assert!(matches!(err, RuntimeError::NotFound(_)), "{err:?}");

    let empty = tempfile::tempdir().unwrap();
    let manager = RuntimeManager::new(RuntimeOptions {
        data_dir: empty.path().to_path_buf(),
        events: EventBus::default(),
        ai_http: Arc::new(HttpClient::new()),
        command_factory: None,
    })
    .await
    .unwrap();
    for role in ServerRole::ALL {
        let err = manager
            .start(role, "x", &settings(30))
            .await
            .expect_err("not installed");
        assert!(matches!(err, RuntimeError::NotInstalled), "{err:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_replacements_preserve_the_running_servers() {
    let h = harness("slow").await;
    for (role, model) in [(CHAT, &h.model_id), (EMBEDDINGS, &h.embedding_model_id)] {
        h.manager.start(role, model, &settings(30)).await.unwrap();
        assert_eq!(
            h.manager
                .wait_settled(role, Duration::from_secs(30))
                .await
                .phase,
            ServerPhase::Running
        );
    }
    let chat = h.manager.server_state(CHAT);
    let embedding = h.manager.server_state(EMBEDDINGS);
    let chat_provider = h.manager.provider(CHAT).unwrap();
    let embedding_provider = h.manager.provider(EMBEDDINGS).unwrap();

    for role in ServerRole::ALL {
        let error = h
            .manager
            .start(role, "missing", &settings(30))
            .await
            .unwrap_err();
        assert!(matches!(error, RuntimeError::NotFound(_)), "{error:?}");
    }

    let missing_file = h.import("removed.gguf", &testing::fake_gguf_bytes()).await;
    std::fs::remove_file(h.data_dir.join("removed.gguf")).unwrap();
    let error = h
        .manager
        .start(CHAT, &missing_file, &settings(30))
        .await
        .unwrap_err();
    assert!(matches!(error, RuntimeError::NotFound(_)), "{error:?}");

    let vision_model = h.import("vision.gguf", &testing::fake_gguf_bytes()).await;
    let projector = h.data_dir.join("vision.mmproj.gguf");
    std::fs::write(&projector, testing::fake_projector_gguf_bytes()).unwrap();
    h.manager
        .attach_projector(&vision_model, &projector)
        .await
        .unwrap();
    std::fs::remove_file(&projector).unwrap();
    let error = h
        .manager
        .start(CHAT, &vision_model, &settings(30))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("vision projector"), "{error}");

    let reranker = h
        .import("reranker.gguf", &testing::fake_gguf(Some(4)))
        .await;
    let error = h
        .manager
        .start(EMBEDDINGS, &reranker, &settings(30))
        .await
        .unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidInput(_)), "{error:?}");

    let planted = h.engine.with_file_name("version.dll");
    std::fs::write(&planted, b"not a real library").unwrap();
    let error = h
        .manager
        .start(CHAT, &h.model_id, &settings(30))
        .await
        .unwrap_err();
    assert!(matches!(error, RuntimeError::BinaryChanged(_)), "{error:?}");
    std::fs::remove_file(planted).unwrap();

    assert_eq!(h.manager.server_state(CHAT), chat);
    assert_eq!(h.manager.server_state(EMBEDDINGS), embedding);
    assert!(Arc::ptr_eq(
        &h.manager.provider(CHAT).unwrap(),
        &chat_provider
    ));
    assert!(Arc::ptr_eq(
        &h.manager.provider(EMBEDDINGS).unwrap(),
        &embedding_provider
    ));
    let reply = chat_provider
        .chat(ChatRequest {
            model: h.model_id.clone(),
            messages: vec![ChatMessage::user("ping")],
            options: GenerationOptions::default(),
        })
        .await
        .unwrap();
    assert_eq!(reply.content, "pong");
    let vectors = embedding_provider
        .embeddings(EmbeddingRequest {
            model: h.embedding_model_id.clone(),
            input: vec!["my cat".into()],
        })
        .await
        .unwrap()
        .vectors;
    assert_eq!(vectors, vec![testing::fake_embedding("my cat")]);
    h.manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn servers_left_by_a_previous_session_are_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let runtime_dir = dir.path().join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let mut orphans = Vec::new();
    for pid_file in ["server.pid", "embedding-server.pid"] {
        let port = jarvis_runtime::process::free_port().unwrap();
        let orphan = std::process::Command::new(&exe)
            .args([
                testing::FAKE_TEST_NAME,
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(FAKE_ENV, testing::mode::SLOW)
            .env("FAKE_PORT", port.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let record = serde_json::json!({
            "pid": orphan.id(),
            "exe": exe.display().to_string(),
            "port": port,
        });
        std::fs::write(runtime_dir.join(pid_file), record.to_string()).unwrap();
        orphans.push(orphan);
    }

    let events = EventBus::default();
    let _manager = RuntimeManager::new(RuntimeOptions {
        data_dir: dir.path().to_path_buf(),
        events: events.clone(),
        ai_http: Arc::new(HttpClient::new()),
        command_factory: None,
    })
    .await
    .unwrap();

    for mut orphan in orphans {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline {
            if orphan.try_wait().unwrap().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Reap the child in every case (no-op if it already exited).
        let _ = orphan.kill();
        let _ = orphan.wait();
        assert!(exited, "the orphaned server must be killed");
    }
    assert!(!runtime_dir.join("server.pid").exists());
    assert!(!runtime_dir.join("embedding-server.pid").exists());
    let recent = events.recent(10);
    assert!(
        recent
            .iter()
            .any(|e| e.message == "Stopped a llama.cpp server left running by a previous session")
    );
    assert!(recent.iter().any(|e| e.message
        == "Stopped a llama.cpp server for embeddings left running by a previous session"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_with_a_projector_sees_images() {
    let h = harness("slow").await;
    // Only mmproj files are accepted as projectors.
    let not_projector = h.data_dir.join("other.gguf");
    std::fs::write(&not_projector, testing::fake_gguf_bytes()).unwrap();
    let err = h
        .manager
        .attach_projector(&h.model_id, &not_projector)
        .await
        .expect_err("not a projector");
    assert!(matches!(err, RuntimeError::InvalidModel(_)), "{err}");

    let projector = h.data_dir.join("vision.mmproj.gguf");
    std::fs::write(&projector, testing::fake_projector_gguf_bytes()).unwrap();
    let model = h
        .manager
        .attach_projector(&h.model_id, &projector)
        .await
        .expect("projector");
    assert!(model.projector.is_some());

    h.manager
        .start(CHAT, &h.model_id, &settings(30))
        .await
        .expect("start");
    let state = h.manager.wait_settled(CHAT, Duration::from_secs(30)).await;
    assert_eq!(state.phase, ServerPhase::Running, "{state:?}");
    assert!(state.vision);
    let models = h
        .manager
        .provider(CHAT)
        .expect("provider")
        .list_models()
        .await
        .expect("models");
    assert!(models[0].capabilities.vision);
    h.manager.stop(CHAT).await;

    // A missing projector is reported before starting.
    std::fs::remove_file(&projector).unwrap();
    let err = h
        .manager
        .start(CHAT, &h.model_id, &settings(30))
        .await
        .expect_err("missing projector");
    assert!(err.to_string().contains("vision projector"), "{err}");
}
