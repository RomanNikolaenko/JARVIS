//! Voice: the core's side of the voice engine (settings, packages,
//! listening, and speaking chat replies).

use jarvis_ai::FinishReason;
use jarvis_events::EventCategory;
use jarvis_voice::{SpeechLanguage, SpeechStream, VoiceEngine, VoiceStatus};

use crate::JarvisCore;
use crate::chat::{ChatEventSink, ChatStreamEvent, SinkClosed};
use crate::config::{AssistantLanguage, Settings};
use crate::error::CoreError;

/// Longest text the "test voice" button may speak.
const MAX_SAY_CHARS: usize = 500;

pub(crate) fn speech_language(language: AssistantLanguage) -> SpeechLanguage {
    match language {
        AssistantLanguage::Uk => SpeechLanguage::Uk,
        AssistantLanguage::En => SpeechLanguage::En,
    }
}

pub(crate) fn apply_settings(voice: &VoiceEngine, settings: &Settings) {
    voice.apply_settings(
        settings.voice.clone(),
        speech_language(settings.general.assistant_language),
    );
}

/// Forwards chat events and reads the reply aloud as it streams in.
pub(crate) struct SpeakingSink<S> {
    pub(crate) inner: S,
    pub(crate) speech: Option<SpeechStream>,
}

impl<S: ChatEventSink> ChatEventSink for SpeakingSink<S> {
    fn send(&mut self, event: ChatStreamEvent) -> Result<(), SinkClosed> {
        if let Some(speech) = &self.speech {
            match &event {
                ChatStreamEvent::Delta { text } => speech.push(text),
                ChatStreamEvent::Finished {
                    finish_reason: FinishReason::Cancelled,
                    ..
                }
                | ChatStreamEvent::Failed { .. } => speech.cancel(),
                ChatStreamEvent::Finished { .. } => speech.finish(),
                ChatStreamEvent::Started { .. } => {}
            }
        }
        self.inner.send(event)
    }
}

impl JarvisCore {
    /// The voice engine, for state, transcript and level subscriptions.
    pub fn voice(&self) -> &VoiceEngine {
        &self.voice
    }

    pub async fn voice_status(&self) -> VoiceStatus {
        self.voice.status().await
    }

    /// Installs a voice package the user approved; returns the download id.
    pub fn voice_install(&self, id: &str) -> Result<String, CoreError> {
        let download = self.voice.install(id)?;
        self.events.info(
            EventCategory::Voice,
            format!("Downloading voice package {id}"),
        );
        Ok(download)
    }

    pub fn voice_cancel_download(&self, id: &str) -> bool {
        self.voice.cancel_download(id)
    }

    /// Deletes an installed voice package (the UI confirms first).
    pub async fn voice_remove(&self, id: &str) -> Result<(), CoreError> {
        self.voice.remove(id).await?;
        self.events
            .info(EventCategory::Voice, format!("Voice package {id} removed"));
        Ok(())
    }

    /// Opens the microphone for one spoken request.
    pub fn voice_listen(&self) -> Result<(), CoreError> {
        self.voice.listen()?;
        Ok(())
    }

    /// Ends the spoken request: `submit` recognizes it, otherwise it is
    /// dropped.
    pub fn voice_stop_listening(&self, submit: bool) {
        self.voice.stop_listening(submit);
    }

    pub fn voice_stop_speaking(&self) {
        self.voice.stop_speaking();
    }

    /// Speaks a short text (to try a voice).
    pub fn voice_say(&self, text: &str) -> Result<(), CoreError> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_SAY_CHARS {
            return Err(CoreError::Validation(format!(
                "The text must have 1 to {MAX_SAY_CHARS} characters"
            )));
        }
        self.voice.say(text);
        Ok(())
    }

    /// Records microphone use in the event log, so it is visible later too.
    pub(crate) fn spawn_voice_events(core: &std::sync::Arc<Self>) {
        let mut state = core.voice.subscribe_state();
        let events = core.events.clone();
        tokio::spawn(async move {
            let mut mic_open = false;
            let mut last_error: Option<String> = None;
            while state.changed().await.is_ok() {
                let current = state.borrow_and_update().clone();
                if current.mic_open != mic_open {
                    mic_open = current.mic_open;
                    let message = match (mic_open, current.wake_word_active) {
                        (true, true) => "Microphone on: listening for the wake word".to_owned(),
                        (true, false) => format!(
                            "Microphone on ({})",
                            current.input_device.as_deref().unwrap_or("default device")
                        ),
                        (false, _) => "Microphone off".to_owned(),
                    };
                    events.info(EventCategory::Voice, message);
                }
                if current.last_error != last_error {
                    last_error.clone_from(&current.last_error);
                    if let Some(error) = &last_error {
                        events.warning(EventCategory::Voice, format!("Voice: {error}"));
                    }
                }
            }
        });
    }
}
