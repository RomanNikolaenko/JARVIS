//! JARVIS voice engine.
//!
//! Independent building blocks — audio I/O ([`audio`]), voice activity
//! detection ([`vad`]), speech recognition with whisper.cpp ([`stt`]),
//! speech synthesis with Piper ([`tts`]) and the wake word ([`wake`]) — and
//! the [`VoiceEngine`] that joins them. Nothing here opens the microphone on
//! its own: capture starts only when the engine is asked to listen or wake
//! word mode is enabled in the settings.

pub mod audio;
pub mod backend;
pub mod catalog;
mod engine;
pub mod error;
pub mod process;
pub mod resample;
pub mod store;
pub mod stt;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod text;
pub mod tts;
pub mod types;
pub mod vad;
pub mod wake;
pub mod wav;

pub use engine::{SpeechLanguage, SpeechStream, VoiceEngine, VoiceEngineOptions, VoiceEvent};
pub use error::{Result, VoiceError};
pub use types::{
    AudioDevice, TranscriptSource, VoiceLanguage, VoicePackage, VoicePackageKind, VoicePhase,
    VoiceSettings, VoiceState, VoiceStatus, VoiceTranscript,
};
