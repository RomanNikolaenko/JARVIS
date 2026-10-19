//! Lifecycle tests of the supervised server with real processes.
//!
//! The test binary doubles as a fake `llama-server`: when started with the
//! `JARVIS_FAKE_LLAMA` environment variable, the `fake_llama_server` test
//! serves a minimal llama.cpp-like HTTP API instead of returning.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jarvis_ai::{ChatMessage, ChatRequest, GenerationOptions, HttpClient};
use jarvis_events::{EventBus, EventCategory};
use jarvis_runtime::process::CommandFactory;
use jarvis_runtime::testing::{self, FAKE_ENV};
use jarvis_runtime::{RuntimeError, RuntimeManager, RuntimeOptions, RuntimeSettings, ServerPhase};

#[test]
fn fake_llama_server() {
    testing::fake_server_entry();
}

fn fake_command(mode: &'static str) -> CommandFactory {
    testing::fake_command_factory(mode)
}

struct Harness {
    manager: Arc<RuntimeManager>,
    events: EventBus,
    model_id: String,
    data_dir: PathBuf,
    _dir: tempfile::TempDir,
}

async fn harness(mode: &'static str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    testing::install_fake_runtime(&data_dir, None)
        .await
        .unwrap();

    let events = EventBus::default();
    let manager = RuntimeManager::new(RuntimeOptions {
        data_dir: data_dir.clone(),
        events: events.clone(),
        ai_http: Arc::new(HttpClient::new()),
        command_factory: Some(fake_command(mode)),
    })
    .await
    .unwrap();

    let model_path = dir.path().join("fake-model.gguf");
    std::fs::write(&model_path, testing::fake_gguf_bytes()).unwrap();
    let model = manager.import_model(&model_path).await.unwrap();
    Harness {
        manager,
        events,
        model_id: model.id,
        data_dir,
        _dir: dir,
    }
}

fn settings(timeout_secs: u32) -> RuntimeSettings {
    RuntimeSettings {
        context_size: 512,
        startup_timeout_secs: timeout_secs,
        ..RuntimeSettings::default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn starts_serves_with_the_api_key_and_stops() {
    let h = harness("slow").await;
    assert!(h.manager.provider().is_none());
    h.manager
        .start(&h.model_id, &settings(30))
        .await
        .expect("start");
    assert_eq!(h.manager.server_state().phase, ServerPhase::Starting);

    let state = h.manager.wait_settled(Duration::from_secs(30)).await;
    assert_eq!(state.phase, ServerPhase::Running, "{state:?}");
    let pid = state.pid.expect("pid");
    assert!(h.data_dir.join("runtime").join("server.pid").exists());

    let provider = h.manager.provider().expect("runtime provider registered");
    assert_eq!(
        provider.descriptor().id.as_str(),
        jarvis_runtime::RUNTIME_PROVIDER_ID
    );
    let models = provider
        .list_models()
        .await
        .expect("models with the API key");
    assert_eq!(models[0].id, h.model_id);
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

    h.manager.stop().await;
    assert_eq!(h.manager.server_state().phase, ServerPhase::Stopped);
    assert!(h.manager.provider().is_none());
    assert!(!h.data_dir.join("runtime").join("server.pid").exists());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        jarvis_platform::process_memory(pid).is_none(),
        "server process must be gone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_failures_report_the_server_log() {
    let h = harness("exit-early").await;
    h.manager
        .start(&h.model_id, &settings(30))
        .await
        .expect("spawned");
    let state = h.manager.wait_settled(Duration::from_secs(30)).await;
    assert_eq!(state.phase, ServerPhase::Failed);
    let error = state.error.expect("error");
    assert!(error.contains("fake failure"), "{error}");
    assert!(h.manager.provider().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn crashes_after_startup_are_detected() {
    let h = harness("crash-after-ready").await;
    h.manager
        .start(&h.model_id, &settings(30))
        .await
        .expect("start");
    assert_eq!(
        h.manager.wait_settled(Duration::from_secs(30)).await.phase,
        ServerPhase::Running
    );

    let mut state = h.manager.subscribe_state();
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| s.phase == ServerPhase::Failed),
    )
    .await
    .expect("crash detected")
    .expect("state channel");
    assert!(
        h.manager.provider().is_none(),
        "a crashed server is unregistered"
    );
    assert!(
        h.manager
            .server_state()
            .error
            .unwrap_or_default()
            .contains("unexpectedly")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_servers_time_out() {
    let h = harness("never-ready").await;
    h.manager
        .start(&h.model_id, &settings(2))
        .await
        .expect("start");
    let state = h.manager.wait_settled(Duration::from_secs(20)).await;
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
    let err = manager
        .start(&h.model_id, &settings(30))
        .await
        .expect_err("tampered binary");
    assert!(matches!(err, RuntimeError::BinaryChanged(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_models_and_missing_runtime_are_reported() {
    let h = harness("slow").await;
    let err = h
        .manager
        .start("missing", &settings(30))
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
    let err = manager
        .start("x", &settings(30))
        .await
        .expect_err("not installed");
    assert!(matches!(err, RuntimeError::NotInstalled), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn servers_left_by_a_previous_session_are_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let port = jarvis_runtime::process::free_port().unwrap();
    let mut orphan = std::process::Command::new(&exe)
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
    let runtime_dir = dir.path().join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let record =
        serde_json::json!({ "pid": orphan.id(), "exe": exe.display().to_string(), "port": port });
    std::fs::write(runtime_dir.join("server.pid"), record.to_string()).unwrap();

    let events = EventBus::default();
    let _manager = RuntimeManager::new(RuntimeOptions {
        data_dir: dir.path().to_path_buf(),
        events: events.clone(),
        ai_http: Arc::new(HttpClient::new()),
        command_factory: None,
    })
    .await
    .unwrap();

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
    assert!(!runtime_dir.join("server.pid").exists());
    assert!(
        events
            .recent(10)
            .iter()
            .any(|e| e.message.contains("previous session"))
    );
}
