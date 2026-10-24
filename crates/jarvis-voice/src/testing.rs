//! Test support (feature `test-support`): a fake microphone, speakers,
//! recognizer and synthesizer, so voice flows can be tested without audio
//! hardware or engines.

// A broken fake must fail the test loudly.
#![allow(clippy::expect_used)]

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::CaptureEvent;
use crate::backend::{InputDevice, OutputDevice, Recognizer, Synthesizer, VoiceBackend};
use crate::catalog::{self, PackageSpec};
use crate::error::{Result, VoiceError};
use crate::store::PackageStore;
use crate::tts::PiperOptions;
use crate::types::AudioDevice;
use crate::vad::SpeechScorer;
use crate::wav::Decoded;

/// Packages [`install_defaults`] marks as installed.
pub const DEFAULT_PACKAGES: &[&str] = &[
    catalog::WHISPER_ENGINE,
    catalog::PIPER_ENGINE,
    "whisper-base-q5_1",
    "piper-uk_UA-ukrainian_tts-medium",
];

/// Marks the engines, a Whisper model and a Ukrainian voice as installed in
/// `voice_dir` without downloading anything. Returns `false` on platforms
/// without voice packages.
pub fn install_defaults(voice_dir: &Path) -> bool {
    if catalog::unsupported_reason().is_some() {
        return false;
    }
    let store = PackageStore::new(voice_dir);
    for id in DEFAULT_PACKAGES {
        let spec = catalog::find(id).expect("catalog package");
        store.mark_installed_for_tests(&spec).expect("test package");
    }
    true
}

/// Frames louder than 0.1 are speech.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoudIsSpeech;

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

/// What the fakes saw and how they behave.
#[derive(Debug, Default)]
pub struct FakeState {
    /// The open microphone's event sender.
    pub mic: Option<mpsc::Sender<CaptureEvent>>,
    pub mic_opened: usize,
    /// What the recognizer answers, in order.
    pub replies: VecDeque<String>,
    /// Lengths of the audio it was asked to recognize.
    pub recognized: Vec<usize>,
    pub languages: Vec<String>,
    pub recognizers_started: usize,
    pub synthesized: Vec<String>,
    pub synthesizer_options: Vec<PiperOptions>,
    pub played: usize,
    pub cleared: usize,
    /// Playback never finishes while set.
    pub hold_playback: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FakeBackend(Arc<Mutex<FakeState>>);

impl FakeBackend {
    pub fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues what the recognizer answers next.
    pub fn reply(&self, text: &str) {
        self.lock().replies.push_back(text.to_owned());
    }

    /// Sends a phrase of `secs` seconds followed by enough silence to end
    /// it, in 20 ms chunks.
    pub async fn speak_into_mic(&self, secs: f32) {
        let mic = self.lock().mic.clone().expect("the microphone is open");
        let speech = (secs * 50.0) as usize;
        for i in 0..speech + 60 {
            let level = if i < speech { 0.5 } else { 0.0 };
            mic.send(CaptureEvent::Audio(vec![level; 320]))
                .await
                .expect("the microphone takes audio");
        }
    }
}

struct FakeMic(FakeBackend);

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

struct FakeSpeaker(FakeBackend);

impl OutputDevice for FakeSpeaker {
    fn enqueue(&self, samples: &[f32], _rate: u32) {
        assert!(!samples.is_empty(), "empty audio queued");
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

struct FakeRecognizer(FakeBackend);

#[async_trait]
impl Recognizer for FakeRecognizer {
    async fn transcribe(&self, samples: &[f32], language: &str) -> Result<String> {
        let mut state = self.0.lock();
        state.recognized.push(samples.len());
        state.languages.push(language.to_owned());
        Ok(state.replies.pop_front().unwrap_or_default())
    }

    fn is_running(&mut self) -> bool {
        true
    }

    async fn stop(self: Box<Self>) {}
}

struct FakeSynthesizer(FakeBackend);

#[async_trait]
impl Synthesizer for FakeSynthesizer {
    async fn synthesize(&mut self, text: &str, cancel: &CancellationToken) -> Result<Decoded> {
        self.0.lock().synthesized.push(text.to_owned());
        tokio::select! {
            () = cancel.cancelled() => return Err(VoiceError::Cancelled),
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
impl VoiceBackend for FakeBackend {
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
        let mut state = self.lock();
        state.mic = Some(events);
        state.mic_opened += 1;
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
