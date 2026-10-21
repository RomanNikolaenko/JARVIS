//! Voice through the core: settings, status and spoken chat replies, with
//! fake audio devices and engines.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use jarvis_core::jarvis_voice::testing::{self, FakeBackend};
use jarvis_core::jarvis_voice::{VoicePackageKind, VoicePhase};
use jarvis_core::{
    AppPaths, ChatStreamEvent, ChatTurnRequest, CoreOptions, ErrorCode, JarvisCore, Settings,
    SinkClosed,
};
use tokio::sync::mpsc;
use uuid::Uuid;

struct TestCore {
    core: Arc<JarvisCore>,
    fake: FakeBackend,
    _dir: tempfile::TempDir,
}

/// A core with the default voice packages "installed", or `None` on
/// platforms without voice packages.
async fn start() -> Option<TestCore> {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        log_dir: dir.path().join("logs"),
    };
    if !testing::install_defaults(&paths.data_dir.join("voice")) {
        return None;
    }
    let fake = FakeBackend::default();
    let core = JarvisCore::initialize(
        CoreOptions::new(paths, "9.9.9")
            .with_provider_factory(None)
            .with_voice_backend(Arc::new(fake.clone())),
    )
    .await
    .unwrap();
    Some(TestCore {
        core,
        fake,
        _dir: dir,
    })
}

async fn turn(test: &TestCore, conversation: Uuid, text: &str, speak: bool) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    test.core
        .send_message(
            ChatTurnRequest {
                conversation_id: conversation,
                content: text.to_owned(),
                provider_id: None,
                model: None,
                temperature: None,
                max_output_tokens: None,
                speak,
            },
            move |event| tx.send(event).map_err(|_| SinkClosed),
        )
        .await
        .unwrap();
    while let Some(event) = rx.recv().await {
        if matches!(
            event,
            ChatStreamEvent::Finished { .. } | ChatStreamEvent::Failed { .. }
        ) {
            break;
        }
    }
}

async fn wait_idle(test: &TestCore) {
    let mut state = test.core.voice().subscribe_state();
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| s.phase == VoicePhase::Idle),
    )
    .await
    .expect("speech ends")
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_to_voice_requests_are_read_aloud() {
    let Some(test) = start().await else { return };
    let conversation = test.core.create_conversation(None, false).await.unwrap();

    // A typed message is not spoken.
    turn(&test, conversation.id, "Котра година?", false).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(test.fake.lock().synthesized.is_empty());

    // A spoken one is (the echo provider repeats it).
    turn(&test, conversation.id, "Яка погода? Дякую.", true).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while test.fake.lock().synthesized.len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the reply is spoken");
    wait_idle(&test).await;
    assert_eq!(
        test.fake.lock().synthesized,
        vec!["[echo] Яка погода?", "Дякую."]
    );

    // With speaking turned off, nothing is read.
    let mut settings = test.core.settings();
    settings.voice.speak_replies = false;
    test.core.update_settings(settings).await.unwrap();
    turn(&test, conversation.id, "Ще раз", true).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(test.fake.lock().synthesized.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_settings_are_validated_and_applied() {
    let Some(test) = start().await else { return };
    let mut settings: Settings = test.core.settings();
    settings.voice.speech_rate = 5.0;
    let err = test
        .core
        .update_settings(settings.clone())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation);
    assert!(err.to_string().contains("voice"), "{err}");

    // Turning the wake word on opens the microphone; off closes it.
    settings.voice.speech_rate = 1.0;
    settings.voice.wake_word = true;
    test.core.update_settings(settings.clone()).await.unwrap();
    let mut state = test.core.voice().subscribe_state();
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| s.mic_open && s.wake_word_active),
    )
    .await
    .expect("microphone opens")
    .unwrap();
    settings.voice.wake_word = false;
    test.core.update_settings(settings).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), state.wait_for(|s| !s.mic_open))
        .await
        .expect("microphone closes")
        .unwrap();
    assert_eq!(test.fake.lock().mic_opened, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_and_short_texts() {
    let Some(test) = start().await else { return };
    let status = test.core.voice_status().await;
    assert!(status.can_listen && status.can_speak, "{status:?}");
    assert!(status.unsupported.is_none());
    assert_eq!(status.input_devices[0].name, "Test microphone");
    let installed: Vec<_> = status
        .packages
        .iter()
        .filter(|p| p.installed)
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(
        installed.len(),
        testing::DEFAULT_PACKAGES.len(),
        "{installed:?}"
    );
    assert!(
        status
            .packages
            .iter()
            .any(|p| p.kind == VoicePackageKind::Voice && !p.installed)
    );

    assert_eq!(
        test.core.voice_say("   ").unwrap_err().code(),
        ErrorCode::Validation
    );
    test.core.voice_say("Перевірка голосу.").unwrap();
    wait_idle(&test).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while test.fake.lock().synthesized.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("spoken");
    assert_eq!(test.fake.lock().synthesized, vec!["Перевірка голосу."]);

    // Unknown packages are rejected before anything is downloaded.
    assert_eq!(
        test.core.voice_install("no-such-voice").unwrap_err().code(),
        ErrorCode::Validation
    );
    test.core.shutdown().await;
}
