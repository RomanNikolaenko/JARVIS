//! Voice settings and the state shown in the UI.

use jarvis_runtime::DownloadStatus;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const MIN_SPEECH_RATE: f32 = 0.5;
pub const MAX_SPEECH_RATE: f32 = 2.0;
pub const MIN_END_SILENCE_MS: u32 = 300;
pub const MAX_END_SILENCE_MS: u32 = 3000;

/// Language of speech recognition and synthesis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub enum VoiceLanguage {
    /// The assistant language from the general settings.
    #[default]
    Auto,
    Uk,
    En,
}

/// Settings → Голос.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "voice.ts")]
pub struct VoiceSettings {
    /// Microphone (device id); `None` uses the system default.
    pub input_device: Option<String>,
    /// Speakers (device id); `None` uses the system default.
    pub output_device: Option<String>,
    pub language: VoiceLanguage,
    /// Whisper model (catalog id); `None` uses the first installed one.
    pub stt_model: Option<String>,
    /// Ukrainian voice (catalog id); `None` uses the first installed one.
    pub voice_uk: Option<String>,
    /// Speaker of a multi-speaker Ukrainian voice.
    pub speaker_uk: u32,
    /// English voice (catalog id); `None` uses the first installed one.
    pub voice_en: Option<String>,
    pub speaker_en: u32,
    /// Read replies to voice requests aloud.
    pub speak_replies: bool,
    /// Keep the microphone open and react to «Джарвіс». Off by default; the
    /// microphone indicator stays on while it is enabled.
    pub wake_word: bool,
    /// 1.0 is the voice's natural pace.
    pub speech_rate: f32,
    /// Silence that ends an utterance.
    pub end_silence_ms: u32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            language: VoiceLanguage::Auto,
            stt_model: None,
            voice_uk: None,
            speaker_uk: 0,
            voice_en: None,
            speaker_en: 0,
            speak_replies: true,
            wake_word: false,
            speech_rate: 1.0,
            end_silence_ms: 800,
        }
    }
}

impl VoiceSettings {
    /// Rejects values outside the supported ranges.
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_SPEECH_RATE..=MAX_SPEECH_RATE).contains(&self.speech_rate) {
            return Err(format!(
                "Speech rate must be between {MIN_SPEECH_RATE} and {MAX_SPEECH_RATE}"
            ));
        }
        if !(MIN_END_SILENCE_MS..=MAX_END_SILENCE_MS).contains(&self.end_silence_ms) {
            return Err(format!(
                "The pause that ends a phrase must be between {MIN_END_SILENCE_MS} and {MAX_END_SILENCE_MS} ms"
            ));
        }
        for id in [
            &self.input_device,
            &self.output_device,
            &self.stt_model,
            &self.voice_uk,
            &self.voice_en,
        ]
        .into_iter()
        .flatten()
        {
            if id.is_empty() || id.len() > 512 {
                return Err("A device or model id is empty or too long".to_owned());
            }
        }
        if self.speaker_uk > 1000 || self.speaker_en > 1000 {
            return Err("Speaker number is out of range".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub struct AudioDevice {
    /// Stable id for the settings.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub enum VoicePhase {
    #[default]
    Idle,
    /// The microphone is open for one request.
    Listening,
    /// Speech is being recognized.
    Transcribing,
    /// A reply is being read aloud.
    Speaking,
}

/// What the voice engine is doing. `mic_open` drives the microphone
/// indicator and is true exactly while audio is being captured.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub struct VoiceState {
    pub phase: VoicePhase,
    pub mic_open: bool,
    /// Wake word mode is on (the microphone stays open).
    pub wake_word_active: bool,
    /// The wake word was heard without a command; the next phrase is one.
    pub awaiting_command: bool,
    /// Voice activity detection hears speech right now.
    pub hearing_speech: bool,
    /// Name of the microphone in use.
    pub input_device: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub enum TranscriptSource {
    /// The user pressed the microphone button.
    PushToTalk,
    /// A phrase that started with the wake word.
    WakeWord,
}

/// Recognized speech meant as a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub struct VoiceTranscript {
    pub text: String,
    pub source: TranscriptSource,
    /// Language whisper used ("uk", "en").
    pub language: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub enum VoicePackageKind {
    /// whisper.cpp speech recognition server.
    SpeechEngine,
    /// Piper speech synthesizer.
    VoiceEngine,
    /// Whisper model.
    SpeechModel,
    /// Piper voice.
    Voice,
}

/// Something that can be downloaded for voice: an engine, a recognition
/// model or a voice. Versions and SHA-256 checksums are pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub struct VoicePackage {
    pub id: String,
    pub kind: VoicePackageKind,
    pub display_name: String,
    pub description: String,
    /// "uk", "en" or `None` for multilingual packages and engines.
    pub language: Option<String>,
    /// Where it comes from (repository and version).
    pub source: String,
    /// Download size in bytes (all files).
    #[ts(type = "number")]
    pub size_bytes: u64,
    pub license: String,
    /// Speaker names of a multi-speaker voice, in id order.
    pub speakers: Vec<String>,
    pub recommended: bool,
    pub installed: bool,
}

/// Everything Settings → Голос shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "voice.ts")]
pub struct VoiceStatus {
    /// Packages offered for this platform.
    pub packages: Vec<VoicePackage>,
    pub downloads: Vec<DownloadStatus>,
    pub input_devices: Vec<AudioDevice>,
    pub output_devices: Vec<AudioDevice>,
    /// Why devices could not be listed (no audio system, permission).
    pub device_error: Option<String>,
    /// Why voice is unavailable on this platform, if it is.
    pub unsupported: Option<String>,
    /// Speech recognition can run (engine and a model installed).
    pub can_listen: bool,
    /// Replies can be spoken (engine and a voice installed).
    pub can_speak: bool,
    pub state: VoiceState,
    pub voice_dir: String,
}
