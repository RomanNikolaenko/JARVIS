//! What the voice engine runs on: audio devices and engine processes. The
//! system backend uses real devices and the installed engines; tests swap
//! in fakes.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::{self, Capture, CaptureEvent, Player};
use crate::catalog::{self, PIPER_ENGINE, PackageSpec, WHISPER_ENGINE};
use crate::error::{Result, VoiceError};
use crate::process::Launcher;
use crate::store::PackageStore;
use crate::stt::WhisperServer;
use crate::tts::{Piper, PiperOptions};
use crate::types::AudioDevice;
use crate::vad::{EarshotScorer, SpeechScorer};
use crate::wav::Decoded;

/// An open microphone; dropping it closes the microphone.
pub trait InputDevice: Send {
    fn name(&self) -> String;
}

/// An open output device.
pub trait OutputDevice: Send + Sync {
    fn enqueue(&self, samples: &[f32], rate: u32);
    fn clear(&self);
    /// Seconds of audio not yet played.
    fn pending_secs(&self) -> f32;
    fn failure(&self) -> Option<String>;
}

#[async_trait]
pub trait Recognizer: Send + Sync {
    async fn transcribe(&self, samples: &[f32], language: &str) -> Result<String>;
    fn is_running(&mut self) -> bool;
    async fn stop(self: Box<Self>);
}

#[async_trait]
pub trait Synthesizer: Send {
    async fn synthesize(&mut self, text: &str, cancel: &CancellationToken) -> Result<Decoded>;
    fn is_running(&mut self) -> bool;
    async fn stop(self: Box<Self>);
}

#[async_trait]
pub trait VoiceBackend: Send + Sync + 'static {
    fn input_devices(&self) -> Result<Vec<AudioDevice>>;
    fn output_devices(&self) -> Result<Vec<AudioDevice>>;
    /// Blocking: waits until the device is open.
    fn open_input(
        &self,
        device: Option<&str>,
        events: mpsc::Sender<CaptureEvent>,
    ) -> Result<Box<dyn InputDevice>>;
    /// Blocking: waits until the device is open.
    fn open_output(&self, device: Option<&str>) -> Result<Box<dyn OutputDevice>>;
    fn scorer(&self) -> Box<dyn SpeechScorer>;
    async fn start_recognizer(&self, model: &PackageSpec) -> Result<Box<dyn Recognizer>>;
    async fn start_synthesizer(
        &self,
        voice: &PackageSpec,
        options: PiperOptions,
    ) -> Result<Box<dyn Synthesizer>>;
}

/// Builds the launcher for an engine program (tests substitute a fake).
pub type LauncherFactory = Arc<dyn Fn(&str, &Path) -> Launcher + Send + Sync>;

pub fn default_launcher_factory() -> LauncherFactory {
    Arc::new(|_engine: &str, program: &Path| Launcher::new(program))
}

pub struct SystemBackend {
    store: PackageStore,
    adapter: Arc<dyn jarvis_platform::PlatformAdapter>,
    launchers: LauncherFactory,
}

impl std::fmt::Debug for SystemBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemBackend")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl SystemBackend {
    pub fn new(
        store: PackageStore,
        adapter: Arc<dyn jarvis_platform::PlatformAdapter>,
        launchers: LauncherFactory,
    ) -> Self {
        Self {
            store,
            adapter,
            launchers,
        }
    }

    async fn launcher(&self, engine_id: &str) -> Result<Launcher> {
        let engine = catalog::find(engine_id).ok_or_else(|| {
            VoiceError::Unsupported(
                catalog::unsupported_reason()
                    .unwrap_or("voice is not available")
                    .to_owned(),
            )
        })?;
        let program = self.store.verified_program(&engine).await?;
        Ok((self.launchers)(engine_id, &program))
    }
}

struct MicHandle(Capture);

impl InputDevice for MicHandle {
    fn name(&self) -> String {
        self.0.device_name().to_owned()
    }
}

impl OutputDevice for Player {
    fn enqueue(&self, samples: &[f32], rate: u32) {
        Player::enqueue(self, samples, rate);
    }
    fn clear(&self) {
        Player::clear(self);
    }
    fn pending_secs(&self) -> f32 {
        Player::pending_secs(self)
    }
    fn failure(&self) -> Option<String> {
        Player::failure(self)
    }
}

#[async_trait]
impl Recognizer for WhisperServer {
    async fn transcribe(&self, samples: &[f32], language: &str) -> Result<String> {
        Ok(WhisperServer::transcribe(self, samples, language)
            .await?
            .text)
    }
    fn is_running(&mut self) -> bool {
        WhisperServer::is_running(self)
    }
    async fn stop(self: Box<Self>) {
        WhisperServer::stop(*self).await;
    }
}

#[async_trait]
impl Synthesizer for Piper {
    async fn synthesize(&mut self, text: &str, cancel: &CancellationToken) -> Result<Decoded> {
        Piper::synthesize(self, text, cancel).await
    }
    fn is_running(&mut self) -> bool {
        Piper::is_running(self)
    }
    async fn stop(self: Box<Self>) {
        Piper::stop(*self).await;
    }
}

#[async_trait]
impl VoiceBackend for SystemBackend {
    fn input_devices(&self) -> Result<Vec<AudioDevice>> {
        audio::input_devices()
    }

    fn output_devices(&self) -> Result<Vec<AudioDevice>> {
        audio::output_devices()
    }

    fn open_input(
        &self,
        device: Option<&str>,
        events: mpsc::Sender<CaptureEvent>,
    ) -> Result<Box<dyn InputDevice>> {
        Ok(Box::new(MicHandle(Capture::start(device, events)?)))
    }

    fn open_output(&self, device: Option<&str>) -> Result<Box<dyn OutputDevice>> {
        Ok(Box::new(Player::open(device)?))
    }

    fn scorer(&self) -> Box<dyn SpeechScorer> {
        Box::new(EarshotScorer::default())
    }

    async fn start_recognizer(&self, model: &PackageSpec) -> Result<Box<dyn Recognizer>> {
        let launcher = self.launcher(WHISPER_ENGINE).await?;
        let path = self.store.file(model, 0).ok_or_else(|| {
            VoiceError::NotInstalled(format!("{} is not installed", model.display_name))
        })?;
        // Leave a core for the audio and the UI.
        let threads =
            std::thread::available_parallelism().map_or(4, |n| n.get().saturating_sub(1).max(1));
        let server = WhisperServer::start(
            &launcher,
            self.store.root(),
            &path,
            threads.min(8),
            self.adapter.as_ref(),
        )
        .await?;
        Ok(Box::new(server))
    }

    async fn start_synthesizer(
        &self,
        voice: &PackageSpec,
        options: PiperOptions,
    ) -> Result<Box<dyn Synthesizer>> {
        let launcher = self.launcher(PIPER_ENGINE).await?;
        let path = self.store.file(voice, 0).ok_or_else(|| {
            VoiceError::NotInstalled(format!("{} is not installed", voice.display_name))
        })?;
        let piper = Piper::start(
            &launcher,
            self.store.root(),
            &path,
            options,
            self.adapter.as_ref(),
        )
        .await?;
        Ok(Box::new(piper))
    }
}
