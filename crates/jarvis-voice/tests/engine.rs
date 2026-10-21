//! The voice engine's flows with a fake microphone, speakers and engines:
//! push-to-talk, the wake word, speaking and interruption.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use jarvis_voice::audio::CaptureEvent;
use jarvis_voice::backend::{InputDevice, OutputDevice, Recognizer, Synthesizer, VoiceBackend};
use jarvis_voice::catalog::{self, PackageSpec};
use jarvis_voice::store::PackageStore;
use jarvis_voice::tts::PiperOptions;
use jarvis_voice::vad::SpeechScorer;
use jarvis_voice::wav::Decoded;
use jarvis_voice::{
    AudioDevice, Result, SpeechLanguage, TranscriptSource, VoiceEngine, VoiceEngineOptions,
    VoicePhase, VoiceSettings, VoiceState, VoiceTranscript,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Frames louder than 0.1 are speech.
struct LoudIsSpeech;

impl SpeechScorer for LoudIsSpeech {
    fn score(&mut self, frame: &[f32]) -> f32 {
        if frame.iter().any(|s| s.abs() > 0.1) {
            1.0
        } else {
            0.0
        }
    }
    fn reset(&mut self) {}
}

#[derive(Default)]
struct Shared {
    /// The open microphone's event sender.
    mic: Option<mpsc::Sender<CaptureEvent>>,
    mic_opened: usize,
    /// What the recognizer answers, in order.
    replies: VecDeque<String>,
    /// Lengths of the audio it was asked to recognize.
    recognized: Vec<usize>,
    recognizers_started: usize,
    synthesized: Vec<String>,
    synthesizer_options: Vec<PiperOptions>,
    played: usize,
    cleared: usize,
    /// Playback never finishes while set.
    hold_playback: bool,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Shared>>);

impl Fake {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.0.lock().unwrap()
    }

    fn reply(&self, text: &str) {
        self.lock().replies.push_back(text.to_owned());
    }

    async fn speak_into_mic(&self, phrase_secs: f32) {
        let mic = self.lock().mic.clone().expect("microphone is open");
        // Speech, then enough silence to end the phrase; 20 ms chunks.
        let speech = (phrase_secs * 50.0) as usize;
        for i in 0..speech + 60 {
            let level = if i < speech { 0.5 } else { 0.0 };
            mic.send(CaptureEvent::Audio(vec![level; 320]))
                .await
                .expect("send");
        }
    }
}

struct FakeMic(Fake);

impl InputDevice for FakeMic {
    fn name(&self) -> String {
        "Test microphone".to_owned()
    }
}

impl Drop for FakeMic {
    fn drop(&mut self) {
        self.0.lock().mic = None;
    }
}

struct FakeSpeaker(Fake);

impl OutputDevice for FakeSpeaker {
    fn enqueue(&self, samples: &[f32], _rate: u32) {
        assert!(!samples.is_empty());
        self.0.lock().played += 1;
    }
    fn clear(&self) {
        self.0.lock().cleared += 1;
    }
    fn pending_secs(&self) -> f32 {
        if self.0.lock().hold_playback {
            1.0
        } else {
            0.0
        }
    }
    fn failure(&self) -> Option<String> {
        None
    }
}

struct FakeRecognizer(Fake);

#[async_trait]
impl Recognizer for FakeRecognizer {
    async fn transcribe(&self, samples: &[f32], language: &str) -> Result<String> {
        assert_eq!(language, "uk");
        let mut shared = self.0.lock();
        shared.recognized.push(samples.len());
        Ok(shared.replies.pop_front().unwrap_or_default())
    }
    fn is_running(&mut self) -> bool {
        true
    }
    async fn stop(self: Box<Self>) {}
}

struct FakeSynthesizer(Fake);

#[async_trait]
impl Synthesizer for FakeSynthesizer {
    async fn synthesize(&mut self, text: &str, cancel: &CancellationToken) -> Result<Decoded> {
        self.0.lock().synthesized.push(text.to_owned());
        tokio::select! {
            () = cancel.cancelled() => return Err(jarvis_voice::VoiceError::Cancelled),
            () = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
        Ok(Decoded {
            samples: vec![0.2; 2205],
            sample_rate: 22_050,
        })
    }
    fn is_running(&mut self) -> bool {
        true
    }
    async fn stop(self: Box<Self>) {}
}

#[async_trait]
impl VoiceBackend for Fake {
    fn input_devices(&self) -> Result<Vec<AudioDevice>> {
        Ok(vec![AudioDevice {
            id: "mic".into(),
            name: "Test microphone".into(),
            is_default: true,
        }])
    }
    fn output_devices(&self) -> Result<Vec<AudioDevice>> {
        Ok(Vec::new())
    }
    fn open_input(
        &self,
        _device: Option<&str>,
        events: mpsc::Sender<CaptureEvent>,
    ) -> Result<Box<dyn InputDevice>> {
        let mut shared = self.lock();
        shared.mic = Some(events);
        shared.mic_opened += 1;
        Ok(Box::new(FakeMic(self.clone())))
    }
    fn open_output(&self, _device: Option<&str>) -> Result<Box<dyn OutputDevice>> {
        Ok(Box::new(FakeSpeaker(self.clone())))
    }
    fn scorer(&self) -> Box<dyn SpeechScorer> {
        Box::new(LoudIsSpeech)
    }
    async fn start_recognizer(&self, _model: &PackageSpec) -> Result<Box<dyn Recognizer>> {
        self.lock().recognizers_started += 1;
        Ok(Box::new(FakeRecognizer(self.clone())))
    }
    async fn start_synthesizer(
        &self,
        _voice: &PackageSpec,
        options: PiperOptions,
    ) -> Result<Box<dyn Synthesizer>> {
        self.lock().synthesizer_options.push(options);
        Ok(Box::new(FakeSynthesizer(self.clone())))
    }
}

struct Setup {
    engine: VoiceEngine,
    fake: Fake,
    state: watch::Receiver<VoiceState>,
    transcripts: broadcast::Receiver<VoiceTranscript>,
    _dir: tempfile::TempDir,
}

/// An engine with everything "installed", or `None` where voice is not
/// offered (other platforms).
fn setup() -> Option<Setup> {
    catalog::unsupported_reason().is_none().then_some(())?;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PackageStore::new(dir.path());
    for id in [
        catalog::WHISPER_ENGINE,
        catalog::PIPER_ENGINE,
        "whisper-base-q5_1",
        "piper-uk_UA-ukrainian_tts-medium",
    ] {
        store
            .mark_installed_for_tests(&catalog::find(id).expect(id))
            .expect("install");
    }
    let fake = Fake::default();
    let engine = VoiceEngine::new(VoiceEngineOptions {
        voice_dir: dir.path().to_path_buf(),
        client: reqwest::Client::new(),
        adapter: Arc::from(jarvis_platform::current_adapter()),
        backend: Some(Arc::new(fake.clone())),
        launchers: None,
    });
    let state = engine.subscribe_state();
    let transcripts = engine.subscribe_transcripts();
    Some(Setup {
        engine,
        fake,
        state,
        transcripts,
        _dir: dir,
    })
}

async fn wait_state(
    state: &mut watch::Receiver<VoiceState>,
    what: &str,
    check: impl Fn(&VoiceState) -> bool,
) {
    let reached = tokio::time::timeout(Duration::from_secs(10), state.wait_for(|s| check(s)))
        .await
        .map(|r| r.is_ok());
    assert!(
        matches!(reached, Ok(true)),
        "timed out waiting for {what}: {:?}",
        *state.borrow()
    );
}

async fn next_transcript(
    transcripts: &mut broadcast::Receiver<VoiceTranscript>,
) -> VoiceTranscript {
    tokio::time::timeout(Duration::from_secs(10), transcripts.recv())
        .await
        .expect("a transcript in time")
        .expect("transcript")
}

fn wake_word_settings() -> VoiceSettings {
    VoiceSettings {
        wake_word: true,
        end_silence_ms: 400,
        ..VoiceSettings::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn push_to_talk_opens_the_microphone_for_one_request() {
    let Some(mut s) = setup() else { return };
    assert!(!s.engine.state().mic_open);
    s.fake.reply("Яка сьогодні погода?");
    s.engine.listen().expect("listen");
    wait_state(&mut s.state, "the microphone", |st| st.mic_open).await;
    assert_eq!(s.engine.state().phase, VoicePhase::Listening);
    assert_eq!(
        s.engine.state().input_device.as_deref(),
        Some("Test microphone")
    );

    s.fake.speak_into_mic(1.0).await;
    let transcript = next_transcript(&mut s.transcripts).await;
    assert_eq!(transcript.text, "Яка сьогодні погода?");
    assert_eq!(transcript.source, TranscriptSource::PushToTalk);
    assert_eq!(transcript.language, "uk");
    // The microphone closed after the phrase.
    wait_state(&mut s.state, "the microphone to close", |st| {
        !st.mic_open && st.phase == VoicePhase::Idle
    })
    .await;
    assert!(s.fake.lock().mic.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn push_to_talk_can_be_submitted_early_or_cancelled() {
    let Some(mut s) = setup() else { return };
    s.fake.reply("Нагадай про зустріч");
    s.engine.listen().expect("listen");
    wait_state(&mut s.state, "the microphone", |st| st.mic_open).await;
    // Speech without the closing silence, then the user presses stop.
    let mic = s.fake.lock().mic.clone().unwrap();
    for _ in 0..40 {
        mic.send(CaptureEvent::Audio(vec![0.5; 320])).await.unwrap();
    }
    wait_state(&mut s.state, "speech", |st| st.hearing_speech).await;
    s.engine.stop_listening(true);
    assert_eq!(
        next_transcript(&mut s.transcripts).await.text,
        "Нагадай про зустріч"
    );

    // Cancelling drops the request without recognizing anything.
    s.engine.listen().expect("listen");
    wait_state(&mut s.state, "the microphone", |st| st.mic_open).await;
    s.engine.stop_listening(false);
    wait_state(&mut s.state, "the microphone to close", |st| !st.mic_open).await;
    assert_eq!(s.fake.lock().recognized.len(), 1);
    assert!(s.engine.state().last_error.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_recognition_installed_listening_is_refused() {
    let Some(s) = setup() else { return };
    s.engine.remove("whisper-base-q5_1").await.expect("remove");
    let err = s.engine.listen().expect_err("no model");
    assert!(err.to_string().contains("Whisper model"), "{err}");
    assert_eq!(
        s.fake.lock().mic_opened,
        0,
        "the microphone must stay closed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wake_word_starts_a_request() {
    let Some(mut s) = setup() else { return };
    s.engine
        .apply_settings(wake_word_settings(), SpeechLanguage::Uk);
    wait_state(&mut s.state, "wake word mode", |st| {
        st.mic_open && st.wake_word_active
    })
    .await;

    // Talk that does not start with the name is ignored.
    s.fake.reply("ну то що там з обідом");
    s.fake.speak_into_mic(1.0).await;
    // The name with a command.
    s.fake.reply("Джарвіс, котра година?");
    s.fake.speak_into_mic(1.0).await;
    let transcript = next_transcript(&mut s.transcripts).await;
    assert_eq!(transcript.text, "котра година?");
    assert_eq!(transcript.source, TranscriptSource::WakeWord);

    // The name alone: the next phrase is the command.
    s.fake.reply("Джарвіс.");
    s.fake.speak_into_mic(0.6).await;
    wait_state(&mut s.state, "waiting for a command", |st| {
        st.awaiting_command
    })
    .await;
    s.fake.reply("увімкни світло");
    s.fake.speak_into_mic(1.0).await;
    assert_eq!(
        next_transcript(&mut s.transcripts).await.text,
        "увімкни світло"
    );
    wait_state(&mut s.state, "the command window to close", |st| {
        !st.awaiting_command
    })
    .await;

    // The microphone stays open until wake word mode is turned off.
    assert!(s.engine.state().mic_open);
    s.engine
        .apply_settings(VoiceSettings::default(), SpeechLanguage::Uk);
    wait_state(&mut s.state, "the microphone to close", |st| {
        !st.mic_open && !st.wake_word_active
    })
    .await;
    assert_eq!(
        s.fake.lock().recognizers_started,
        1,
        "the recognizer is reused"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn long_background_talk_is_checked_by_its_beginning_only() {
    let Some(mut s) = setup() else { return };
    s.engine
        .apply_settings(wake_word_settings(), SpeechLanguage::Uk);
    wait_state(&mut s.state, "wake word mode", |st| st.mic_open).await;
    s.fake.reply("а потім ми поїхали");
    s.fake.speak_into_mic(6.0).await;
    // Then a short call proves the first phrase was handled.
    s.fake.reply("Джарвіс, стоп");
    s.fake.speak_into_mic(1.0).await;
    assert_eq!(next_transcript(&mut s.transcripts).await.text, "стоп");
    let recognized = s.fake.lock().recognized.clone();
    assert_eq!(recognized.len(), 2, "{recognized:?}");
    assert_eq!(
        recognized[0],
        16_000 * 3,
        "only the first 3 s of the long phrase"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_reply_is_spoken_sentence_by_sentence() {
    let Some(mut s) = setup() else { return };
    let mut settings = VoiceSettings {
        speaker_uk: 2,
        speech_rate: 1.25,
        ..VoiceSettings::default()
    };
    s.engine
        .apply_settings(settings.clone(), SpeechLanguage::Uk);
    assert!(s.engine.speaks_replies());
    let speech = s.engine.speak();
    speech.push("Добрий день! Сьогодні ");
    speech.push("сонячно.\n```\nкод не читаємо\n```\nГотово");
    speech.finish();
    wait_state(&mut s.state, "speaking", |st| {
        st.phase == VoicePhase::Speaking
    })
    .await;
    wait_state(&mut s.state, "the end of speech", |st| {
        st.phase == VoicePhase::Idle
    })
    .await;
    {
        let shared = s.fake.lock();
        assert_eq!(
            shared.synthesized,
            vec!["Добрий день!", "Сьогодні сонячно.", "Готово"]
        );
        assert_eq!(shared.played, 3);
        let options = shared.synthesizer_options[0];
        assert_eq!(options.speaker, Some(2));
        assert!((options.rate - 1.25).abs() < 1e-6);
    }

    settings.speak_replies = false;
    s.engine.apply_settings(settings, SpeechLanguage::Uk);
    assert!(!s.engine.speaks_replies());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wake_word_interrupts_speech() {
    let Some(mut s) = setup() else { return };
    s.engine
        .apply_settings(wake_word_settings(), SpeechLanguage::Uk);
    wait_state(&mut s.state, "wake word mode", |st| st.mic_open).await;
    s.fake.lock().hold_playback = true;
    let speech = s.engine.speak();
    speech.push("Це дуже довга відповідь, яку ніхто не дослухає. ");
    wait_state(&mut s.state, "speaking", |st| {
        st.phase == VoicePhase::Speaking
    })
    .await;

    s.fake.reply("Джарвіс, зупинись і відкрий нотатки");
    s.fake.speak_into_mic(1.5).await;
    assert_eq!(
        next_transcript(&mut s.transcripts).await.text,
        "зупинись і відкрий нотатки"
    );
    wait_state(&mut s.state, "silence", |st| st.phase == VoicePhase::Idle).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(s.fake.lock().cleared >= 1, "queued audio must be dropped");
}

#[tokio::test(flavor = "multi_thread")]
async fn push_to_talk_stops_speech_first() {
    let Some(mut s) = setup() else { return };
    s.fake.lock().hold_playback = true;
    s.engine.say("Довга відповідь. Ще одне речення.");
    wait_state(&mut s.state, "speaking", |st| {
        st.phase == VoicePhase::Speaking
    })
    .await;
    s.engine.listen().expect("listen");
    wait_state(&mut s.state, "listening", |st| {
        st.phase == VoicePhase::Listening && st.mic_open
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(s.fake.lock().cleared >= 1);
    s.engine.stop_listening(false);
    s.engine.shutdown().await;
}
