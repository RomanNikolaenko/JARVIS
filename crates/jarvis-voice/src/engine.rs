//! The voice engine: listening (push-to-talk and the wake word), speech
//! recognition, and reading replies aloud with interruption.
//!
//! The microphone is opened only by [`VoiceEngine::listen`] or while wake
//! word mode is enabled in the settings, and `VoiceState::mic_open` is true
//! exactly while it is open, so the UI can always show it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use jarvis_runtime::download::Downloads;
use jarvis_runtime::{DownloadKind, RuntimeError};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::audio::CaptureEvent;
use crate::backend::{
    LauncherFactory, OutputDevice, Recognizer, Synthesizer, SystemBackend, VoiceBackend,
    default_launcher_factory,
};
use crate::catalog::{self, PIPER_ENGINE, PackageSpec, WHISPER_ENGINE};
use crate::error::{Result, VoiceError};
use crate::store::PackageStore;
use crate::text::SentenceSplitter;
use crate::tts::PiperOptions;
use crate::types::{
    TranscriptSource, VoiceLanguage, VoicePackageKind, VoicePhase, VoiceSettings, VoiceState,
    VoiceStatus, VoiceTranscript,
};
use crate::vad::{SAMPLE_RATE, SegmentEvent, Segmenter, SegmenterConfig};
use crate::wake;

/// Push-to-talk gives up when nobody speaks for this long.
const NO_SPEECH_TIMEOUT: Duration = Duration::from_secs(10);
/// After a bare «Джарвіс», the next phrase within this time is the command.
const COMMAND_WINDOW: Duration = Duration::from_secs(10);
/// Long phrases are first checked for the wake word on their beginning only.
const WAKE_PROBE_SAMPLES: usize = SAMPLE_RATE as usize * 3;
const LEVEL_INTERVAL: Duration = Duration::from_millis(66);
const PLAYBACK_POLL: Duration = Duration::from_millis(30);

/// Language for recognition and synthesis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpeechLanguage {
    #[default]
    Uk,
    En,
}

impl SpeechLanguage {
    pub fn code(self) -> &'static str {
        match self {
            Self::Uk => "uk",
            Self::En => "en",
        }
    }

    /// The language a text is written in, judged by its letters.
    fn of_text(text: &str, fallback: Self) -> Self {
        let (mut cyrillic, mut latin) = (0usize, 0usize);
        for c in text.chars().filter(|c| c.is_alphabetic()) {
            if ('\u{0400}'..='\u{04FF}').contains(&c) {
                cyrillic += 1;
            } else if c.is_ascii_alphabetic() {
                latin += 1;
            }
        }
        // Ukrainian replies often quote English names and commands.
        if cyrillic == 0 && latin == 0 {
            fallback
        } else if cyrillic * 2 >= latin {
            Self::Uk
        } else {
            Self::En
        }
    }
}

pub struct VoiceEngineOptions {
    /// Where voice packages are installed.
    pub voice_dir: PathBuf,
    pub client: reqwest::Client,
    pub adapter: Arc<dyn jarvis_platform::PlatformAdapter>,
    /// Replaces the audio devices and engine processes (tests).
    pub backend: Option<Arc<dyn VoiceBackend>>,
    /// Replaces how engine programs are started (tests).
    pub launchers: Option<LauncherFactory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenMode {
    PushToTalk,
    WakeWord,
}

#[derive(Debug)]
enum Control {
    /// Push-to-talk: the user finished speaking.
    Submit,
    /// Wake word mode: treat the next phrase as a command.
    ExpectCommand,
}

#[derive(Debug)]
struct Listener {
    id: u64,
    mode: ListenMode,
    cancel: CancellationToken,
    control: mpsc::UnboundedSender<Control>,
}

#[derive(Debug, Clone, PartialEq)]
struct Config {
    settings: VoiceSettings,
    assistant_language: SpeechLanguage,
}

#[derive(Debug, Clone, PartialEq)]
struct SynthKey {
    voice: String,
    speaker: Option<u32>,
    rate: f32,
}

enum SpeechInput {
    Text(String),
    Finish,
}

/// Text for one spoken reply, fed as it streams in.
#[derive(Debug, Clone)]
pub struct SpeechStream {
    tx: mpsc::UnboundedSender<SpeechInput>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for SpeechInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Text(_) => "Text",
            Self::Finish => "Finish",
        })
    }
}

impl SpeechStream {
    pub fn push(&self, text: &str) {
        let _ = self.tx.send(SpeechInput::Text(text.to_owned()));
    }

    /// The reply is complete; the rest is spoken and the stream ends.
    pub fn finish(&self) {
        let _ = self.tx.send(SpeechInput::Finish);
    }

    /// Stops speaking this reply.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

struct Inner {
    backend: Arc<dyn VoiceBackend>,
    store: PackageStore,
    downloads: Downloads,
    client: reqwest::Client,
    config: Mutex<Config>,
    state: watch::Sender<VoiceState>,
    transcripts: broadcast::Sender<VoiceTranscript>,
    levels: broadcast::Sender<f32>,
    recognizer: tokio::sync::Mutex<Option<(String, Box<dyn Recognizer>)>>,
    synthesizer: tokio::sync::Mutex<Option<(SynthKey, Box<dyn Synthesizer>)>>,
    listener: Mutex<Option<Listener>>,
    speech: Mutex<Option<(u64, CancellationToken)>>,
    command_deadline: Mutex<Option<Instant>>,
    installing: Mutex<HashSet<String>>,
    next_id: AtomicU64,
    /// The most recently started listener; older ones leave the state alone.
    latest_listener: AtomicU64,
    shutdown: CancellationToken,
}

#[derive(Clone)]
pub struct VoiceEngine {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for VoiceEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceEngine")
            .field("voice_dir", &self.inner.store.root())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

impl VoiceEngine {
    pub fn new(options: VoiceEngineOptions) -> Self {
        let store = PackageStore::new(options.voice_dir);
        let backend = options.backend.unwrap_or_else(|| {
            Arc::new(SystemBackend::new(
                store.clone(),
                options.adapter,
                options.launchers.unwrap_or_else(default_launcher_factory),
            ))
        });
        let (state, _) = watch::channel(VoiceState::default());
        Self {
            inner: Arc::new(Inner {
                backend,
                store,
                downloads: Downloads::default(),
                client: options.client,
                config: Mutex::new(Config {
                    settings: VoiceSettings::default(),
                    assistant_language: SpeechLanguage::Uk,
                }),
                state,
                transcripts: broadcast::channel(16).0,
                levels: broadcast::channel(32).0,
                recognizer: tokio::sync::Mutex::new(None),
                synthesizer: tokio::sync::Mutex::new(None),
                listener: Mutex::new(None),
                speech: Mutex::new(None),
                command_deadline: Mutex::new(None),
                installing: Mutex::new(HashSet::new()),
                next_id: AtomicU64::new(1),
                latest_listener: AtomicU64::new(0),
                shutdown: CancellationToken::new(),
            }),
        }
    }

    pub fn subscribe_state(&self) -> watch::Receiver<VoiceState> {
        self.inner.state.subscribe()
    }

    pub fn state(&self) -> VoiceState {
        self.inner.state.borrow().clone()
    }

    /// Recognized requests (push-to-talk and wake word).
    pub fn subscribe_transcripts(&self) -> broadcast::Receiver<VoiceTranscript> {
        self.inner.transcripts.subscribe()
    }

    /// Microphone loudness (0–1), about 15 times a second while it is open.
    pub fn subscribe_levels(&self) -> broadcast::Receiver<f32> {
        self.inner.levels.subscribe()
    }

    fn config(&self) -> Config {
        lock(&self.inner.config).clone()
    }

    fn language(&self) -> SpeechLanguage {
        let config = self.config();
        match config.settings.language {
            VoiceLanguage::Auto => config.assistant_language,
            VoiceLanguage::Uk => SpeechLanguage::Uk,
            VoiceLanguage::En => SpeechLanguage::En,
        }
    }

    fn update(&self, change: impl FnOnce(&mut VoiceState)) {
        self.inner.state.send_if_modified(|state| {
            let before = state.clone();
            change(state);
            *state != before
        });
    }

    fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    // ----- settings and status -----

    /// Applies new settings: wake word mode starts or stops, and the
    /// engines restart with new models or voices when next used.
    pub fn apply_settings(&self, settings: VoiceSettings, assistant_language: SpeechLanguage) {
        let previous = std::mem::replace(
            &mut *lock(&self.inner.config),
            Config {
                settings: settings.clone(),
                assistant_language,
            },
        );
        let capture_changed = previous.settings.input_device != settings.input_device
            || previous.settings.end_silence_ms != settings.end_silence_ms
            || previous.settings.language != settings.language
            || previous.assistant_language != assistant_language;
        if capture_changed && self.listening_mode() == Some(ListenMode::WakeWord) {
            self.stop_listener();
        }
        self.refresh_wake_word();
    }

    fn listening_mode(&self) -> Option<ListenMode> {
        lock(&self.inner.listener).as_ref().map(|l| l.mode)
    }

    /// Starts or stops wake word listening to match the settings.
    fn refresh_wake_word(&self) {
        let wanted = self.config().settings.wake_word && !self.inner.shutdown.is_cancelled();
        match (wanted, self.listening_mode()) {
            (true, None) => match self.speech_model() {
                Ok(_) => self.start_listener(ListenMode::WakeWord),
                Err(err) => self.update(|s| {
                    s.wake_word_active = false;
                    s.last_error = Some(format!("Wake word: {err}"));
                }),
            },
            (false, Some(ListenMode::WakeWord)) => self.stop_listener(),
            _ => {}
        }
    }

    fn installed(&self, kind: VoicePackageKind) -> Vec<PackageSpec> {
        let mut found: Vec<PackageSpec> = catalog::packages()
            .into_iter()
            .filter(|p| p.kind == kind && self.inner.store.is_installed(p))
            .collect();
        // Recommended ones first, otherwise catalog order.
        found.sort_by_key(|p| !p.recommended);
        found
    }

    fn engine_ready(&self, id: &str) -> bool {
        catalog::find(id).is_some_and(|spec| self.inner.store.is_installed(&spec))
    }

    /// The Whisper model to use.
    fn speech_model(&self) -> Result<PackageSpec> {
        if let Some(reason) = catalog::unsupported_reason() {
            return Err(VoiceError::Unsupported(reason.to_owned()));
        }
        if !self.engine_ready(WHISPER_ENGINE) {
            return Err(VoiceError::NotInstalled(
                "speech recognition is not installed (whisper.cpp in Settings → Voice)".into(),
            ));
        }
        let models = self.installed(VoicePackageKind::SpeechModel);
        let wanted = self.config().settings.stt_model;
        wanted
            .and_then(|id| models.iter().find(|m| m.id == id).cloned())
            .or_else(|| models.into_iter().next())
            .ok_or_else(|| {
                VoiceError::NotInstalled("no Whisper model is installed (Settings → Voice)".into())
            })
    }

    /// The voice and speaker for `language`.
    fn voice_for(&self, language: SpeechLanguage) -> Result<(PackageSpec, Option<u32>)> {
        if let Some(reason) = catalog::unsupported_reason() {
            return Err(VoiceError::Unsupported(reason.to_owned()));
        }
        if !self.engine_ready(PIPER_ENGINE) {
            return Err(VoiceError::NotInstalled(
                "speech synthesis is not installed (Piper in Settings → Voice)".into(),
            ));
        }
        let settings = self.config().settings;
        let (wanted, speaker) = match language {
            SpeechLanguage::Uk => (settings.voice_uk, settings.speaker_uk),
            SpeechLanguage::En => (settings.voice_en, settings.speaker_en),
        };
        let voices = self.installed(VoicePackageKind::Voice);
        let voice = wanted
            .and_then(|id| voices.iter().find(|v| v.id == id).cloned())
            .or_else(|| {
                voices
                    .iter()
                    .find(|v| v.language == Some(language.code()))
                    .cloned()
            })
            .or_else(|| voices.into_iter().next())
            .ok_or_else(|| {
                VoiceError::NotInstalled("no voice is installed (Settings → Voice)".into())
            })?;
        let speaker =
            (!voice.speakers.is_empty()).then(|| speaker.min(voice.speakers.len() as u32 - 1));
        Ok((voice, speaker))
    }

    pub async fn status(&self) -> VoiceStatus {
        let backend = Arc::clone(&self.inner.backend);
        let devices = tokio::task::spawn_blocking(move || {
            (backend.input_devices(), backend.output_devices())
        })
        .await;
        let (input_devices, output_devices, device_error) = match devices {
            Ok((Ok(inputs), Ok(outputs))) => (inputs, outputs, None),
            Ok((inputs, outputs)) => {
                let error = inputs
                    .as_ref()
                    .err()
                    .or(outputs.as_ref().err())
                    .map(ToString::to_string);
                (
                    inputs.unwrap_or_default(),
                    outputs.unwrap_or_default(),
                    error,
                )
            }
            Err(err) => (Vec::new(), Vec::new(), Some(err.to_string())),
        };
        let packages = catalog::packages()
            .iter()
            .map(|p| p.describe(self.inner.store.is_installed(p)))
            .collect();
        VoiceStatus {
            packages,
            downloads: self.inner.downloads.snapshot(),
            input_devices,
            output_devices,
            device_error,
            unsupported: catalog::unsupported_reason().map(str::to_owned),
            can_listen: self.speech_model().is_ok(),
            can_speak: self.voice_for(self.language()).is_ok(),
            state: self.state(),
            voice_dir: self.inner.store.root().display().to_string(),
        }
    }

    // ----- packages -----

    /// Downloads and installs a package the user approved. Returns the
    /// download id; progress is in [`VoiceEngine::status`].
    pub fn install(&self, id: &str) -> Result<String> {
        let spec = catalog::find(id)
            .ok_or_else(|| VoiceError::Invalid(format!("unknown voice package {id}")))?;
        if !lock(&self.inner.installing).insert(spec.id.to_owned()) {
            return Err(VoiceError::Busy(format!(
                "{} is already being installed",
                spec.display_name
            )));
        }
        let handle = self
            .inner
            .downloads
            .begin(DownloadKind::Voice, spec.display_name);
        let download_id = handle.id().to_owned();
        let engine = self.clone();
        tokio::spawn(async move {
            // Running engines hold their files open (Windows cannot replace them).
            engine.stop_processes().await;
            let result = engine
                .inner
                .store
                .install(&spec, &engine.inner.client, &handle)
                .await;
            let report = result.as_ref().map(|_| ()).map_err(|err| match err {
                VoiceError::Cancelled => RuntimeError::Cancelled,
                other => RuntimeError::Download(other.to_string()),
            });
            handle.finish(&report);
            if let Err(err) = &result {
                tracing::warn!("installing {} failed: {err}", spec.id);
            }
            lock(&engine.inner.installing).remove(spec.id);
            engine.refresh_wake_word();
        });
        Ok(download_id)
    }

    pub fn cancel_download(&self, id: &str) -> bool {
        self.inner.downloads.cancel(id)
    }

    /// Deletes an installed package (the UI asks the user first).
    pub async fn remove(&self, id: &str) -> Result<()> {
        let spec = catalog::find(id)
            .ok_or_else(|| VoiceError::Invalid(format!("unknown voice package {id}")))?;
        if lock(&self.inner.installing).contains(spec.id) {
            return Err(VoiceError::Busy(format!(
                "{} is being installed",
                spec.display_name
            )));
        }
        self.stop_speaking();
        if self.listening_mode() == Some(ListenMode::WakeWord) {
            self.stop_listener();
        }
        self.stop_processes().await;
        self.inner.store.remove(spec.id).await?;
        self.refresh_wake_word();
        Ok(())
    }

    async fn stop_processes(&self) {
        if let Some((_, recognizer)) = self.inner.recognizer.lock().await.take() {
            recognizer.stop().await;
        }
        if let Some((_, synthesizer)) = self.inner.synthesizer.lock().await.take() {
            synthesizer.stop().await;
        }
    }

    // ----- recognition -----

    async fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let model = self.speech_model()?;
        let language = self.language();
        let mut slot = self.inner.recognizer.lock().await;
        let reuse = match slot.as_mut() {
            Some((id, recognizer)) => *id == model.id && recognizer.is_running(),
            None => false,
        };
        if !reuse {
            if let Some((_, old)) = slot.take() {
                old.stop().await;
            }
            let recognizer = self.inner.backend.start_recognizer(&model).await?;
            *slot = Some((model.id.to_owned(), recognizer));
        }
        let Some((_, recognizer)) = slot.as_ref() else {
            return Err(VoiceError::Engine(
                "speech recognition is not running".into(),
            ));
        };
        recognizer.transcribe(samples, language.code()).await
    }

    fn emit(&self, text: String, source: TranscriptSource) {
        let transcript = VoiceTranscript {
            text,
            source,
            language: self.language().code().to_owned(),
        };
        let _ = self.inner.transcripts.send(transcript);
    }

    // ----- listening -----

    /// Opens the microphone for one request (push-to-talk). Speaking stops.
    /// With wake word mode on, the next phrase is taken as a command.
    pub fn listen(&self) -> Result<()> {
        self.stop_speaking();
        {
            let listener = lock(&self.inner.listener);
            if let Some(active) = listener.as_ref() {
                if active.mode == ListenMode::WakeWord {
                    let _ = active.control.send(Control::ExpectCommand);
                }
                return Ok(());
            }
        }
        self.speech_model()?;
        self.start_listener(ListenMode::PushToTalk);
        Ok(())
    }

    /// Push-to-talk: `submit` recognizes what was said so far; otherwise
    /// the request is dropped.
    pub fn stop_listening(&self, submit: bool) {
        let listener = lock(&self.inner.listener);
        if let Some(active) = listener
            .as_ref()
            .filter(|l| l.mode == ListenMode::PushToTalk)
        {
            if submit {
                let _ = active.control.send(Control::Submit);
            } else {
                active.cancel.cancel();
            }
        }
    }

    fn stop_listener(&self) {
        if let Some(active) = lock(&self.inner.listener).take() {
            active.cancel.cancel();
        }
    }

    fn is_latest_listener(&self, id: u64) -> bool {
        self.inner.latest_listener.load(Ordering::Relaxed) == id
    }

    /// Frees the listener slot so a new request can start.
    fn release_listener(&self, id: u64) {
        let mut slot = lock(&self.inner.listener);
        if slot.as_ref().is_some_and(|l| l.id == id) {
            *slot = None;
        }
    }

    fn start_listener(&self, mode: ListenMode) {
        let id = self.next_id();
        let cancel = self.inner.shutdown.child_token();
        let (control, control_rx) = mpsc::unbounded_channel();
        *lock(&self.inner.listener) = Some(Listener {
            id,
            mode,
            cancel: cancel.clone(),
            control,
        });
        self.inner.latest_listener.store(id, Ordering::Relaxed);
        self.update(|s| {
            s.last_error = None;
            s.wake_word_active = mode == ListenMode::WakeWord;
            if mode == ListenMode::PushToTalk {
                s.phase = VoicePhase::Listening;
            }
        });
        let engine = self.clone();
        tokio::spawn(async move {
            let result = engine.run_listener(id, mode, &cancel, control_rx).await;
            cancel.cancel();
            engine.release_listener(id);
            // A newer listener owns the state now.
            if !engine.is_latest_listener(id) {
                return;
            }
            *lock(&engine.inner.command_deadline) = None;
            engine.update(|s| {
                s.mic_open = false;
                s.hearing_speech = false;
                s.awaiting_command = false;
                s.wake_word_active = false;
                s.input_device = None;
                if matches!(s.phase, VoicePhase::Listening | VoicePhase::Transcribing) {
                    s.phase = VoicePhase::Idle;
                }
            });
            match result {
                Ok(()) | Err(VoiceError::Cancelled) => {}
                Err(err) => {
                    tracing::warn!("voice listening ended: {err}");
                    engine.update(|s| s.last_error = Some(err.to_string()));
                }
            }
        });
    }

    async fn run_listener(
        &self,
        id: u64,
        mode: ListenMode,
        cancel: &CancellationToken,
        mut control: mpsc::UnboundedReceiver<Control>,
    ) -> Result<()> {
        let settings = self.config().settings;
        let (events, mut audio) = mpsc::channel::<CaptureEvent>(256);
        let backend = Arc::clone(&self.inner.backend);
        let device = settings.input_device.clone();
        let input =
            tokio::task::spawn_blocking(move || backend.open_input(device.as_deref(), events))
                .await
                .map_err(|err| VoiceError::Device(err.to_string()))??;
        if cancel.is_cancelled() {
            return Err(VoiceError::Cancelled);
        }
        self.update(|s| {
            s.mic_open = true;
            s.input_device = Some(input.name());
        });

        let mut segmenter = Segmenter::with_scorer(
            self.inner.backend.scorer(),
            SegmenterConfig {
                end_silence_ms: settings.end_silence_ms,
                ..SegmenterConfig::default()
            },
        );
        // Wake word mode: phrases go to a worker so capture never stalls;
        // when it is busy, new phrases are dropped.
        let (phrases, phrases_rx) = mpsc::channel::<Vec<f32>>(2);
        let worker = (mode == ListenMode::WakeWord).then(|| {
            let engine = self.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move { engine.run_wake_worker(phrases_rx, &cancel).await })
        });

        let started = Instant::now();
        let mut heard = false;
        let mut last_level = Instant::now();
        let outcome: Result<Option<Vec<f32>>> = loop {
            tokio::select! {
                () = cancel.cancelled() => break Err(VoiceError::Cancelled),
                Some(message) = control.recv() => match message {
                    Control::Submit => {
                        // Everything captured so far belongs to the request.
                        let mut finished = None;
                        while let Ok(CaptureEvent::Audio(samples)) = audio.try_recv() {
                            for event in segmenter.push(&samples) {
                                if let SegmentEvent::Finished(phrase) = event {
                                    finished.get_or_insert(phrase);
                                }
                            }
                        }
                        break Ok(finished.or_else(|| segmenter.flush()));
                    }
                    Control::ExpectCommand => self.expect_command(),
                },
                event = audio.recv() => {
                    let samples = match event {
                        Some(CaptureEvent::Audio(samples)) => samples,
                        Some(CaptureEvent::Failed(err)) => break Err(VoiceError::Device(err)),
                        None => break Err(VoiceError::Device("the microphone stopped".into())),
                    };
                    let mut finished = None;
                    for event in segmenter.push(&samples) {
                        match event {
                            SegmentEvent::Started => {
                                heard = true;
                                self.update(|s| s.hearing_speech = true);
                            }
                            SegmentEvent::Finished(phrase) => {
                                self.update(|s| s.hearing_speech = false);
                                if mode == ListenMode::PushToTalk {
                                    finished = Some(phrase);
                                } else {
                                    let _ = phrases.try_send(phrase);
                                }
                            }
                        }
                    }
                    if finished.is_some() {
                        break Ok(finished);
                    }
                    if last_level.elapsed() >= LEVEL_INTERVAL {
                        last_level = Instant::now();
                        let _ = self.inner.levels.send(segmenter.level());
                    }
                    if mode == ListenMode::PushToTalk && !heard && started.elapsed() > NO_SPEECH_TIMEOUT {
                        break Ok(None);
                    }
                }
            }
        };

        // Close the microphone before the (slow) recognition; a new request
        // may start meanwhile.
        drop(input);
        drop(phrases);
        if let Some(worker) = worker {
            worker.abort();
        }
        self.release_listener(id);
        if self.is_latest_listener(id) {
            self.update(|s| {
                s.mic_open = false;
                s.hearing_speech = false;
                s.input_device = None;
            });
            let _ = self.inner.levels.send(0.0);
        }

        let phrase = match outcome? {
            Some(phrase) => phrase,
            None if mode == ListenMode::PushToTalk => {
                return Err(VoiceError::Invalid("no speech was heard".into()));
            }
            None => return Ok(()),
        };
        if self.is_latest_listener(id) {
            self.update(|s| s.phase = VoicePhase::Transcribing);
        }
        let text = tokio::select! {
            () = cancel.cancelled() => return Err(VoiceError::Cancelled),
            text = self.transcribe(&phrase) => text?,
        };
        if self.is_latest_listener(id) {
            self.update(|s| s.phase = VoicePhase::Idle);
        }
        if !has_words(&text) {
            return Err(VoiceError::Invalid(
                "the speech could not be recognized".into(),
            ));
        }
        self.emit(text, TranscriptSource::PushToTalk);
        Ok(())
    }

    async fn run_wake_worker(
        &self,
        mut phrases: mpsc::Receiver<Vec<f32>>,
        cancel: &CancellationToken,
    ) {
        loop {
            let phrase = tokio::select! {
                () = cancel.cancelled() => return,
                phrase = phrases.recv() => match phrase {
                    Some(phrase) => phrase,
                    None => return,
                },
            };
            match self.handle_wake_phrase(&phrase).await {
                Ok(()) | Err(VoiceError::Cancelled) => {}
                Err(err) => {
                    tracing::warn!("wake word recognition failed: {err}");
                    self.update(|s| s.last_error = Some(err.to_string()));
                }
            }
        }
    }

    fn command_pending(&self) -> bool {
        lock(&self.inner.command_deadline).is_some_and(|deadline| Instant::now() < deadline)
    }

    fn expect_command(&self) {
        let deadline = Instant::now() + COMMAND_WINDOW;
        *lock(&self.inner.command_deadline) = Some(deadline);
        self.update(|s| s.awaiting_command = true);
        let engine = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(COMMAND_WINDOW).await;
            let expired = {
                let mut slot = lock(&engine.inner.command_deadline);
                let expired = slot.is_some_and(|d| d == deadline);
                if expired {
                    *slot = None;
                }
                expired
            };
            if expired {
                engine.update(|s| s.awaiting_command = false);
            }
        });
    }

    fn command_done(&self) {
        *lock(&self.inner.command_deadline) = None;
        self.update(|s| s.awaiting_command = false);
    }

    async fn handle_wake_phrase(&self, phrase: &[f32]) -> Result<()> {
        if self.command_pending() {
            let text = self.transcribe(phrase).await?;
            self.command_done();
            // The command may repeat the wake word.
            let command = wake::find(&text).map_or(text, |found| found.command);
            if has_words(&command) {
                self.emit(command, TranscriptSource::WakeWord);
            }
            return Ok(());
        }
        // Background talk rarely starts with «Джарвіс»: check the start of
        // long phrases before recognizing all of them.
        if phrase.len() > WAKE_PROBE_SAMPLES + SAMPLE_RATE as usize {
            let probe = self.transcribe(&phrase[..WAKE_PROBE_SAMPLES]).await?;
            if wake::find(&probe).is_none() {
                return Ok(());
            }
        }
        let text = self.transcribe(phrase).await?;
        let Some(found) = wake::find(&text) else {
            return Ok(());
        };
        // Barge-in: the user wants JARVIS's attention.
        self.stop_speaking();
        if has_words(&found.command) {
            self.emit(found.command, TranscriptSource::WakeWord);
        } else {
            self.expect_command();
            self.chime().await;
        }
        Ok(())
    }

    /// A short rising tone: «I'm listening».
    async fn chime(&self) {
        let rate = 16_000.0;
        let mut samples = Vec::new();
        for (frequency, seconds) in [(660.0_f32, 0.09_f32), (880.0, 0.12)] {
            let count = (rate * seconds) as usize;
            for i in 0..count {
                let fade = (i.min(count - i) as f32 / (rate * 0.008)).min(1.0);
                samples
                    .push((std::f32::consts::TAU * frequency * i as f32 / rate).sin() * 0.2 * fade);
            }
        }
        let backend = Arc::clone(&self.inner.backend);
        let device = self.config().settings.output_device;
        let _ = tokio::task::spawn_blocking(move || {
            let output = backend.open_output(device.as_deref())?;
            output.enqueue(&samples, 16_000);
            std::thread::sleep(Duration::from_millis(400));
            Ok::<_, VoiceError>(())
        })
        .await;
    }

    // ----- speaking -----

    /// Starts speaking a reply that arrives in pieces. Any current speech
    /// stops.
    pub fn speak(&self) -> SpeechStream {
        self.stop_speaking();
        let id = self.next_id();
        let cancel = self.inner.shutdown.child_token();
        *lock(&self.inner.speech) = Some((id, cancel.clone()));
        let (tx, rx) = mpsc::unbounded_channel();
        let engine = self.clone();
        let token = cancel.clone();
        tokio::spawn(async move {
            let mut output = None;
            let result = engine.run_speech(rx, &token, &mut output).await;
            if let Some(output) = output.take() {
                if result.is_err() {
                    output.clear();
                }
                drop(output);
            }
            let current = {
                let mut slot = lock(&engine.inner.speech);
                let current = slot.as_ref().is_some_and(|(i, _)| *i == id);
                if current {
                    *slot = None;
                }
                current
            };
            if current {
                engine.update(|s| {
                    if s.phase == VoicePhase::Speaking {
                        s.phase = VoicePhase::Idle;
                    }
                });
            }
            match result {
                Ok(()) | Err(VoiceError::Cancelled) => {}
                Err(err) => {
                    tracing::warn!("speaking failed: {err}");
                    engine.update(|s| s.last_error = Some(err.to_string()));
                }
            }
        });
        SpeechStream { tx, cancel }
    }

    /// Speaks a complete text.
    pub fn say(&self, text: &str) {
        let stream = self.speak();
        stream.push(text);
        stream.finish();
    }

    pub fn stop_speaking(&self) {
        if let Some((_, cancel)) = lock(&self.inner.speech).take() {
            cancel.cancel();
        }
        self.update(|s| {
            if s.phase == VoicePhase::Speaking {
                s.phase = VoicePhase::Idle;
            }
        });
    }

    /// Whether replies to voice requests should be spoken.
    pub fn speaks_replies(&self) -> bool {
        self.config().settings.speak_replies && self.voice_for(self.language()).is_ok()
    }

    async fn run_speech(
        &self,
        mut input: mpsc::UnboundedReceiver<SpeechInput>,
        cancel: &CancellationToken,
        output: &mut Option<Box<dyn OutputDevice>>,
    ) -> Result<()> {
        let mut splitter = SentenceSplitter::new();
        let mut language = None;
        let mut finished = false;
        while !finished {
            let message = tokio::select! {
                () = cancel.cancelled() => return Err(VoiceError::Cancelled),
                message = input.recv() => message,
            };
            let sentences = match message {
                Some(SpeechInput::Text(text)) => splitter.push(&text),
                Some(SpeechInput::Finish) | None => {
                    finished = true;
                    splitter.finish()
                }
            };
            for sentence in sentences {
                if cancel.is_cancelled() {
                    return Err(VoiceError::Cancelled);
                }
                let language = *language
                    .get_or_insert_with(|| SpeechLanguage::of_text(&sentence, self.language()));
                let audio = self.synthesize(&sentence, language, cancel).await?;
                if output.is_none() {
                    let backend = Arc::clone(&self.inner.backend);
                    let device = self.config().settings.output_device;
                    let opened =
                        tokio::task::spawn_blocking(move || backend.open_output(device.as_deref()))
                            .await
                            .map_err(|err| VoiceError::Device(err.to_string()))??;
                    *output = Some(opened);
                }
                if cancel.is_cancelled() {
                    return Err(VoiceError::Cancelled);
                }
                self.update(|s| s.phase = VoicePhase::Speaking);
                if let Some(output) = output.as_ref() {
                    output.enqueue(&audio.samples, audio.sample_rate);
                }
            }
        }
        // Let the queued audio play out.
        if let Some(output) = output.as_ref() {
            loop {
                if let Some(failure) = output.failure() {
                    return Err(VoiceError::Device(failure));
                }
                if output.pending_secs() <= 0.0 {
                    break;
                }
                tokio::select! {
                    () = cancel.cancelled() => return Err(VoiceError::Cancelled),
                    () = tokio::time::sleep(PLAYBACK_POLL) => {}
                }
            }
            // The device's own buffer.
            tokio::select! {
                () = cancel.cancelled() => return Err(VoiceError::Cancelled),
                () = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
        Ok(())
    }

    async fn synthesize(
        &self,
        text: &str,
        language: SpeechLanguage,
        cancel: &CancellationToken,
    ) -> Result<crate::wav::Decoded> {
        let (voice, speaker) = self.voice_for(language)?;
        let key = SynthKey {
            voice: voice.id.to_owned(),
            speaker,
            rate: self.config().settings.speech_rate,
        };
        let mut slot = self.inner.synthesizer.lock().await;
        let reuse = match slot.as_mut() {
            Some((current, synthesizer)) => *current == key && synthesizer.is_running(),
            None => false,
        };
        if !reuse {
            if let Some((_, old)) = slot.take() {
                old.stop().await;
            }
            let options = PiperOptions {
                speaker,
                rate: key.rate,
            };
            let synthesizer = self
                .inner
                .backend
                .start_synthesizer(&voice, options)
                .await?;
            *slot = Some((key, synthesizer));
        }
        let Some((_, synthesizer)) = slot.as_mut() else {
            return Err(VoiceError::Engine("speech synthesis is not running".into()));
        };
        synthesizer.synthesize(text, cancel).await
    }

    /// Stops listening, speaking, downloads and the engine processes.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        self.inner.downloads.cancel_all();
        self.stop_listener();
        self.stop_speaking();
        self.stop_processes().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_of_a_text_follows_its_letters() {
        assert_eq!(
            SpeechLanguage::of_text("Привіт, як справи?", SpeechLanguage::En),
            SpeechLanguage::Uk
        );
        assert_eq!(
            SpeechLanguage::of_text("Hello there", SpeechLanguage::Uk),
            SpeechLanguage::En
        );
        assert_eq!(
            SpeechLanguage::of_text("Запусти cargo test", SpeechLanguage::En),
            SpeechLanguage::Uk
        );
        assert_eq!(
            SpeechLanguage::of_text("42", SpeechLanguage::En),
            SpeechLanguage::En
        );
    }
}
